//! 插件自己的凭据（protocol-v1 §6.2）：宿主只管存和还，内容由插件定。重新上线要的全在里面——用户授权（访问令牌、
//! 刷新令牌、过期时刻、权限），和登录方式（通过中继：中继地址；自己的应用：App ID 与 App Secret，宿主把整份凭据存进
//! 系统凭据库）。另记上次核验的账号，只用来核对。
//!
//! 附录 D 的交接（`hhh-builtin-v1`）也在这里换成插件的凭据：HHH 内置飞书的授权与连接设置那一栏。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::relay;
use crate::{AuthGrant, ImResult};

/// 凭据格式的版本。
const VERSION: u32 = 1;
/// 离过期不足 5 分钟就换新（同 Friend 的 `REFRESH_MARGIN_SECS`：覆盖一次查询的最长耗时，又不会每次请求都续）。
pub(crate) const REFRESH_MARGIN_SECS: u64 = 300;

/// 登录方式：登录与续期都按它走。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "method",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum Login {
    /// 通过中继（中继上配好的飞书应用，中继代换令牌与续期）；地址为空是默认中继。
    Relay {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        relay_url: Option<String>,
    },
    /// 用户自己的应用（本机回调、本机换令牌与续期）。
    Own { app_id: String, app_secret: String },
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Relay { relay_url } => write!(f, "Relay({relay_url:?})"),
            Self::Own { app_id, .. } => write!(f, "Own({app_id}, secret: [已隐藏])"),
        }
    }
}

impl Login {
    /// 登录方式在 `loginMethods` 里的编号。
    pub(crate) fn method_id(&self) -> &'static str {
        match self {
            Self::Relay { .. } => "relay",
            Self::Own { .. } => "own",
        }
    }

    /// 用的中继地址（默认中继时是默认地址）。
    pub(crate) fn relay_base(&self) -> Option<String> {
        match self {
            Self::Relay { relay_url } => Some(
                relay_url
                    .clone()
                    .unwrap_or_else(|| relay::DEFAULT_RELAY.to_owned()),
            ),
            Self::Own { .. } => None,
        }
    }
}

/// 上次核验的账号（只用来核对与写日志）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KnownAccount {
    pub(crate) id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
}

/// 插件的凭据。不实现给人看的 Debug：令牌与密钥不进日志。
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Credential {
    version: u32,
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    /// 访问令牌的过期时刻（Unix 秒，同内置时代）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scopes: Option<Vec<String>>,
    pub(crate) login: Login,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) account: Option<KnownAccount>,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("tokens", &"[已隐藏]")
            .field("expires_at", &self.expires_at)
            .field("login", &self.login)
            .finish()
    }
}

impl Credential {
    /// 用一份授权与登录方式拼凭据。
    pub(crate) fn new(grant: &AuthGrant, login: Login, account: Option<KnownAccount>) -> Self {
        Self {
            version: VERSION,
            access_token: grant.access_token().to_owned(),
            refresh_token: grant.refresh_token().map(str::to_owned),
            expires_at: grant.expires_at(),
            scopes: grant.scopes().map(<[String]>::to_vec),
            login,
            account,
        }
    }

    /// 读宿主还回来的凭据；读不懂时为空。
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let credential: Self = serde_json::from_str(text).ok()?;
        (credential.version == VERSION).then_some(credential)
    }

    /// 写成交给宿主的文字。
    pub(crate) fn to_text(&self) -> String {
        serde_json::to_string(self).expect("凭据总能写成 JSON")
    }

    /// 里面的用户授权。
    pub(crate) fn grant(&self) -> ImResult<AuthGrant> {
        AuthGrant::from_parts(
            self.access_token.clone(),
            self.refresh_token.clone(),
            self.expires_at,
            self.scopes.clone(),
        )
    }

    /// 换了一份授权（续期）：登录方式与账号照旧。
    pub(crate) fn with_grant(&self, grant: &AuthGrant) -> Self {
        Self::new(grant, self.login.clone(), self.account.clone())
    }

    /// 访问令牌（认“是不是同一份授权”用）。
    pub(crate) fn access_token(&self) -> &str {
        &self.access_token
    }

    /// 下次该续期的时间（毫秒）：过期前 5 分钟；没有刷新令牌、不知道过期时间时不排续期。
    pub(crate) fn renew_at_ms(&self) -> Option<i64> {
        self.refresh_token.as_ref()?;
        let at = self.expires_at?.saturating_sub(REFRESH_MARGIN_SECS);
        i64::try_from(at).ok()?.checked_mul(1000)
    }
}

/// 授权在使用前的状况（同 Friend 的 `grant::freshness`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Freshness {
    /// 有效且不临近过期；过期时间未知时也交给平台判定。
    Current,
    /// 仍有效但临近过期：尽量换新，临时失败时继续用旧的。
    RefreshSoon,
    /// 已过期：必须换新才能使用。
    MustRefresh,
    /// 已过期且没有刷新令牌：只能重新登录。
    Unrenewable,
}

/// 按过期时间与有没有刷新令牌判断要不要换新。
pub(crate) fn freshness(grant: &AuthGrant, now: u64) -> Freshness {
    let Some(expires_at) = grant.expires_at() else {
        return Freshness::Current;
    };
    let renewable = grant.refresh_token().is_some();
    if expires_at <= now {
        if renewable {
            Freshness::MustRefresh
        } else {
            Freshness::Unrenewable
        }
    } else if renewable && expires_at <= now.saturating_add(REFRESH_MARGIN_SECS) {
        Freshness::RefreshSoon
    } else {
        Freshness::Current
    }
}

/// 交接来的内置授权（附录 D 的 `credential`）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyGrant {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_at: Option<u64>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
    #[serde(default)]
    account: Option<LegacyAccount>,
}

#[derive(Deserialize)]
struct LegacyAccount {
    id: String,
    #[serde(default)]
    name: Option<String>,
}

/// HHH 内置飞书在连接设置里的那一栏（附录 D 的 `settings`）：`mode` 只在中继登录时写（没有就是自己的应用），
/// `relayUrl` 没有是默认中继；密钥 `appSecret` 已换成明文。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacySettings {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    relay_url: Option<String>,
    #[serde(default)]
    app_id: Option<String>,
    #[serde(default)]
    app_secret: Option<String>,
}

/// 交接换成插件的凭据；内置那份用不了（读不懂、自己的应用缺 App ID 或 Secret）时返回给用户看的原因。
pub(crate) fn from_legacy(credential: &str, settings: &Value) -> Result<Credential, String> {
    let grant: LegacyGrant = serde_json::from_str(credential)
        .map_err(|_| "交接来的飞书授权读不懂，请重新登录。".to_owned())?;
    let settings: LegacySettings = serde_json::from_value(settings.clone())
        .map_err(|_| "交接来的飞书连接设置读不懂，请重新登录。".to_owned())?;
    let blank =
        |value: Option<String>| value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    let login = if settings.mode.as_deref() == Some("relay") {
        Login::Relay {
            relay_url: blank(settings.relay_url).and_then(|url| relay::stored_relay_url(&url)),
        }
    } else {
        match (blank(settings.app_id), blank(settings.app_secret)) {
            (Some(app_id), Some(app_secret)) => Login::Own { app_id, app_secret },
            _ => return Err("飞书应用配置不完整，请在设置里重新登录。".into()),
        }
    };
    let grant_check = AuthGrant::from_parts(
        grant.access_token.clone(),
        grant.refresh_token.clone(),
        grant.expires_at,
        grant.scopes.clone(),
    )
    .map_err(|error| error.message)?;
    Ok(Credential::new(
        &grant_check,
        login,
        grant.account.map(|account| KnownAccount {
            id: account.id,
            name: account.name,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn credentials_round_trip_and_hide_their_tokens() {
        let grant = AuthGrant::from_parts(
            "u-access".into(),
            Some("ur-refresh".into()),
            Some(10_000),
            Some(vec!["im:message".into()]),
        )
        .unwrap();
        let credential = Credential::new(
            &grant,
            Login::Own {
                app_id: "cli_1".into(),
                app_secret: "s3cret".into(),
            },
            None,
        );
        let text = credential.to_text();
        let back = Credential::parse(&text).unwrap();
        assert_eq!(back.grant().unwrap().refresh_token(), Some("ur-refresh"));
        assert_eq!(back.login, credential.login);
        assert_eq!(back.renew_at_ms(), Some((10_000 - 300) * 1000));
        let shown = format!("{credential:?}");
        assert!(
            !shown.contains("u-access")
                && !shown.contains("ur-refresh")
                && !shown.contains("s3cret")
        );
        assert!(Credential::parse("{\"version\":9}").is_none());
    }

    // 附录 D：中继登录写 mode、默认中继不写地址；自己的应用不写 mode，App ID 与明文的 Secret 都要有。
    #[test]
    fn legacy_handovers_keep_the_login_mode_and_the_grant() {
        let legacy =
            json!({"accessToken": "at", "refreshToken": "rt", "expiresAt": 1_791_090_000u64,
            "scopes": ["offline_access"], "account": {"id": "ou_me", "name": "本人"}})
            .to_string();
        let relay = from_legacy(&legacy, &json!({"mode": "relay", "accountName": "本人"})).unwrap();
        assert_eq!(relay.login, Login::Relay { relay_url: None });
        assert_eq!(relay.account.as_ref().unwrap().id, "ou_me");
        assert_eq!(relay.grant().unwrap().expires_at(), Some(1_791_090_000));
        let custom = from_legacy(
            &legacy,
            &json!({"mode": "relay", "relayUrl": "https://relay.example"}),
        )
        .unwrap();
        assert_eq!(
            custom.login,
            Login::Relay {
                relay_url: Some("https://relay.example".into())
            }
        );
        let default = from_legacy(
            &legacy,
            &json!({"mode": "relay", "relayUrl": relay::DEFAULT_RELAY}),
        )
        .unwrap();
        assert_eq!(
            default.login,
            Login::Relay { relay_url: None },
            "等于默认中继记成默认"
        );
        let own = from_legacy(&legacy, &json!({"appId": "cli_1", "appSecret": "s"})).unwrap();
        assert_eq!(
            own.login,
            Login::Own {
                app_id: "cli_1".into(),
                app_secret: "s".into()
            }
        );
        assert_eq!(
            from_legacy(&legacy, &json!({"appId": "cli_1"})).unwrap_err(),
            "飞书应用配置不完整，请在设置里重新登录。"
        );
        assert!(from_legacy("{}", &json!({})).is_err());
    }

    #[test]
    fn freshness_follows_friend() {
        let grant = |refresh: bool, expires: u64| {
            AuthGrant::from_parts("a".into(), refresh.then(|| "r".into()), Some(expires), None)
                .unwrap()
        };
        assert_eq!(freshness(&grant(true, 10_000), 1_000), Freshness::Current);
        assert_eq!(
            freshness(&grant(true, 1_200), 1_000),
            Freshness::RefreshSoon
        );
        assert_eq!(freshness(&grant(false, 1_200), 1_000), Freshness::Current);
        assert_eq!(freshness(&grant(true, 900), 1_000), Freshness::MustRefresh);
        assert_eq!(freshness(&grant(false, 900), 1_000), Freshness::Unrenewable);
    }
}
