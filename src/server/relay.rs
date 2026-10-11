//! 飞书“通过中继登录”用的公网授权回调中继（从 HHH Friend 的 `im/auth_relay.rs` 搬来，契约 friend/im-auth-relay v2、v5）。
//!
//! 飞书没有 PKCE、换码要 App Secret，所以登记时要求中继代换令牌（Secret 只在中继上），领到的直接是令牌，之后的续期
//! 也经中继。插件先用一次性 state 在中继登记，成功后才把授权页交给宿主打开，之后按中继建议的间隔领取结果。
//! 领取凭证、授权码与令牌只在内存，不写日志。协议正本在独立仓库 hhh-im-callback-server 的 README。
//!
//! 和 Friend 那份的不同：没有测试旋钮（中继地址由宿主在登录框里交来，默认中继写在插件的登录方式里）；只留飞书用到的部分。

use std::time::{Duration, Instant};

use reqwest::{StatusCode, Url};
use serde::Deserialize;

use crate::{AuthGrant, ErrorKind, ImError, ImResult, ReauthReason};

/// 正式中继；用户与飞书后台登记的都是它下面的 `/hhh-friend-im/feishu/callback`。
pub(crate) const DEFAULT_RELAY: &str = "https://hhh-im-auth.magiclizi.com";
/// 在中继上登记、领取用的平台名。
const PLATFORM: &str = "feishu";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// 中继建议的等待间隔落在这个范围里；没给或给得离谱时按默认 2 秒。
pub(crate) const RETRY_DEFAULT: Duration = Duration::from_secs(2);
const RETRY_MIN: Duration = Duration::from_millis(500);
const RETRY_MAX: Duration = Duration::from_secs(10);
/// 中继给的中文说明最多显示这么多字符，防止异常响应把提示撑满。
const RELAY_MESSAGE_MAX_CHARS: usize = 120;
/// 中继没配飞书应用时给用户的话（契约 friend/im-auth-relay v5 F6）。
const FEISHU_UNAVAILABLE: &str = "这个中继没有配置飞书应用，可以改用自己的应用。";

/// 一个中继：来源地址与出站客户端（统一 TLS，不跟随跳转）。
#[derive(Clone)]
pub(crate) struct Relay {
    base: Url,
    http: reqwest::Client,
}

/// 在中继登记好的一次登录：state 与领取凭证只在内存；Debug 不显示它们。
pub(crate) struct Flow {
    state: String,
    claim_token: String,
    expires_at: Instant,
}

impl std::fmt::Debug for Flow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Flow([领取凭证已隐藏])")
    }
}

impl Flow {
    /// 中继给的有效期已过（之后再领取只会拿到“已过期”）。
    pub(crate) fn expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }
}

/// 领取一次的结果。
pub(crate) enum Claim {
    /// 浏览器还没回来；按建议间隔再来。
    Pending { retry_after: Duration },
    /// 平台给了授权码（应当领到令牌；领到码说明中继没按要求代换）。
    Code,
    /// 平台给了错误（用户拒绝等）。
    Denied,
    /// 中继已用授权码换到令牌。
    Token(RelayToken),
    /// 飞书拒绝了换令牌（带飞书错误码）。
    ExchangeFailed { message: String },
}

impl std::fmt::Debug for Claim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending { retry_after } => write!(f, "Pending({retry_after:?})"),
            Self::Code => f.write_str("Code([已隐藏])"),
            Self::Denied => f.write_str("Denied"),
            Self::Token(_) => f.write_str("Token([已隐藏])"),
            Self::ExchangeFailed { .. } => f.write_str("ExchangeFailed"),
        }
    }
}

/// 中继转来的飞书令牌（只有飞书回执里这几项）；Debug 不显示令牌。
#[derive(Deserialize)]
pub(crate) struct RelayToken {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: u64,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
}

impl RelayToken {
    /// 换成授权（过期时间按现在算），同读飞书回执的规则：要有令牌与正的有效期，类型只认 Bearer。
    pub(crate) fn into_grant(self, now_secs: u64) -> Result<AuthGrant, String> {
        if self.expires_in == 0 {
            return Err("中继转来的飞书令牌没有有效期。".into());
        }
        if self
            .token_type
            .as_deref()
            .is_some_and(|kind| !kind.eq_ignore_ascii_case("bearer"))
        {
            return Err("中继转来的飞书令牌类型不受支持。".into());
        }
        AuthGrant::from_parts(
            self.access_token,
            self.refresh_token,
            Some(now_secs.saturating_add(self.expires_in)),
            self.scope
                .map(|scope| scope.split_whitespace().map(str::to_owned).collect()),
        )
        .map_err(|error| error.to_string())
    }
}

/// 中继出错：`message` 是给用户的中文；`gone` 表示这次登录在中继那边已不存在（过期、已领取、被清理），
/// 再领取也没用；`transient` 表示网络或服务暂时问题，等待中可以下次再试。
#[derive(Debug, Clone)]
pub(crate) struct RelayError {
    pub(crate) message: String,
    pub(crate) gone: bool,
    pub(crate) transient: bool,
}

impl RelayError {
    fn unreachable(cause: String) -> Self {
        Self {
            message: format!("授权回调服务暂时连不上（{cause}），请稍后再试。"),
            gone: false,
            transient: true,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Created {
    state: String,
    claim_token: String,
    callback_url: String,
    expires_in: u64,
    /// 代换令牌的飞书会话：中继的飞书 App ID（拼授权地址用）。
    #[serde(default)]
    client_id: Option<String>,
}

/// 登记好的一次登录，另带中继的 App ID。
pub(crate) struct Registered {
    pub(crate) flow: Flow,
    pub(crate) client_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Claimed {
    status: String,
    #[serde(default)]
    retry_after_ms: Option<u64>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    result: Option<ClaimedResult>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum ClaimedResult {
    /// 授权码（`code` 不读：飞书的码本机换不了）。
    Code {},
    /// 平台给的错误（`error`、`errorDescription` 不读：一律算用户取消）。
    Error {},
    Token {
        token: RelayToken,
    },
    // 中继协议原样写作 `exchange_error`（契约 server/feishu-exchange v2）；飞书错误码 `code` 不读。
    #[serde(rename = "exchange_error")]
    ExchangeError {
        #[serde(default)]
        message: String,
    },
}

#[derive(Deserialize)]
struct Refreshed {
    token: RelayToken,
}

#[derive(Deserialize)]
struct RelayFailure {
    error: RelayFailureBody,
}

#[derive(Deserialize)]
struct RelayFailureBody {
    code: String,
    message: String,
    /// 续期被飞书拒绝时带飞书错误码。
    #[serde(default, rename = "feishuCode")]
    feishu_code: Option<i64>,
}

/// 中继来源的规则：只要来源（不带路径、参数、账号）；https，或只在本机回环上的 http（测试中继）。
/// 出错时给用户看的原因（登录框里的中继地址）。
pub(crate) fn parse_relay_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw.trim())
        .map_err(|_| "中继地址要是完整的网址，以 https:// 开头。".to_owned())?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(url.scheme() == "https" || (url.scheme() == "http" && loopback)) {
        return Err("中继地址要用 https（本机测试可以用 http://127.0.0.1）。".into());
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err("中继地址只填网址本身，不带路径或参数。".into());
    }
    Ok(url)
}

/// 用户填的中继地址规整成来源（`https://host[:port]`，不带结尾斜杠）。
pub(crate) fn normalize_relay_url(raw: &str) -> Result<String, String> {
    parse_relay_url(raw).map(|url| url.as_str().trim_end_matches('/').to_owned())
}

/// 凭据里记的中继地址：等于默认中继时记成“默认”（默认地址变了也跟着变，同 Friend）；认不出的地址当默认。
pub(crate) fn stored_relay_url(raw: &str) -> Option<String> {
    normalize_relay_url(raw)
        .ok()
        .filter(|url| url != DEFAULT_RELAY)
}

impl Relay {
    /// 一个中继（地址已核对过）。
    pub(crate) fn new(raw: &str) -> Result<Self, String> {
        let base = parse_relay_url(raw)?;
        let http = crate::net::client_builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| {
                let cause = crate::net::network_cause(error);
                tracing::error!(%cause, "IM 授权中继客户端建立失败");
                format!("授权回调服务客户端建立失败：{cause}")
            })?;
        Ok(Self { base, http })
    }

    /// 登记在飞书后台的回调地址。
    pub(crate) fn callback_url(&self) -> String {
        format!(
            "{}/hhh-friend-im/{PLATFORM}/callback",
            self.base.as_str().trim_end_matches('/')
        )
    }

    fn endpoint(&self, path: &str) -> Url {
        // 基址只有来源、路径固定由这里拼，join 不会出错。
        self.base.join(path).expect("中继来源拼固定路径")
    }

    /// 用插件生成的 state 登记一次登录，要求中继代换飞书令牌（回执里带中继的 App ID）。中继回的 state 与回调地址
    /// 必须与本机一致，否则说明中继的公网地址配置不符，登记作废（不能把用户带去一个回不来的地址）。
    pub(crate) async fn register(&self, state: &str) -> Result<Registered, RelayError> {
        let started = Instant::now();
        let body = serde_json::json!({ "platform": PLATFORM, "state": state, "exchange": true });
        let response = self
            .http
            .post(self.endpoint("/v1/flows"))
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                let cause = crate::net::network_cause(error);
                tracing::warn!(platform = PLATFORM, %cause, "IM 授权中继登记失败：连不上");
                RelayError::unreachable(cause)
            })?;
        let status = response.status();
        if status != StatusCode::CREATED {
            return Err(failure("登记", status, response).await);
        }
        let created: Created = response.json().await.map_err(|error| {
            let cause = crate::net::network_cause(error);
            tracing::warn!(platform = PLATFORM, %cause, "IM 授权中继登记回执读不懂");
            RelayError {
                message: format!("授权回调服务的回执读不懂（{cause}），请稍后再试。"),
                gone: false,
                transient: true,
            }
        })?;
        if created.state != state || created.callback_url != self.callback_url() {
            tracing::error!(
                platform = PLATFORM,
                "IM 授权中继回的回调地址或 state 与本机不一致"
            );
            return Err(RelayError {
                message: "授权回调服务的地址配置与 Friend 不一致，暂时不能登录。".into(),
                gone: false,
                transient: false,
            });
        }
        let Some(client_id) = created.client_id.filter(|id| !id.trim().is_empty()) else {
            tracing::error!(
                platform = PLATFORM,
                "IM 授权中继没有回飞书 App ID，可能还不支持代换令牌"
            );
            return Err(RelayError {
                message: "这个中继还不支持飞书代换令牌，请更新中继，或改用自己的应用。".into(),
                gone: false,
                transient: false,
            });
        };
        tracing::info!(
            platform = PLATFORM,
            exchange = true,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "IM 授权已在中继登记"
        );
        Ok(Registered {
            flow: Flow {
                state: created.state,
                claim_token: created.claim_token,
                expires_at: Instant::now() + Duration::from_secs(created.expires_in),
            },
            client_id,
        })
    }

    /// 领取一次。令牌只出现在返回值里，不进日志。
    pub(crate) async fn claim(&self, flow: &Flow) -> Result<Claim, RelayError> {
        let path = format!("/v1/flows/{}/claim", flow.state);
        let response = self
            .http
            .post(self.endpoint(&path))
            .bearer_auth(&flow.claim_token)
            .send()
            .await
            .map_err(|error| {
                let cause = crate::net::network_cause(error);
                tracing::warn!(platform = PLATFORM, %cause, "IM 授权中继领取失败：连不上");
                RelayError::unreachable(cause)
            })?;
        let status = response.status();
        if status != StatusCode::OK {
            return Err(failure("领取", status, response).await);
        }
        let claimed: Claimed = response.json().await.map_err(|error| {
            let cause = crate::net::network_cause(error);
            tracing::warn!(platform = PLATFORM, %cause, "IM 授权中继领取回执读不懂");
            RelayError {
                message: format!("授权回调服务的回执读不懂（{cause}），请重新登录。"),
                gone: false,
                transient: true,
            }
        })?;
        match (claimed.status.as_str(), claimed.result) {
            ("pending", _) => Ok(Claim::Pending {
                retry_after: claimed
                    .retry_after_ms
                    .map(Duration::from_millis)
                    .map_or(RETRY_DEFAULT, |wait| wait.clamp(RETRY_MIN, RETRY_MAX)),
            }),
            ("completed", Some(result)) if claimed.state.as_deref() == Some(&flow.state) => {
                Ok(match result {
                    ClaimedResult::Code { .. } => Claim::Code,
                    ClaimedResult::Error { .. } => Claim::Denied,
                    ClaimedResult::Token { token } => Claim::Token(token),
                    ClaimedResult::ExchangeError { message } => Claim::ExchangeFailed {
                        message: clean(&message),
                    },
                })
            }
            _ => {
                tracing::error!(
                    platform = PLATFORM,
                    "IM 授权中继领取回执的状态不认识或 state 不符"
                );
                Err(RelayError {
                    message: "授权回调服务给的结果对不上这次登录，请重新登录。".into(),
                    gone: true,
                    transient: false,
                })
            }
        }
    }

    /// 经中继续期飞书授权（契约 friend/im-auth-relay v5 F7）：刷新令牌只用这一次，不重试。
    /// 只有飞书拒绝（中继回 `feishu_refresh_rejected`）按“登录已失效”；中继或飞书暂时连不上、中继暂时没配飞书应用、
    /// 限频，以及中继或它前面的代理回的其它错误（如请求体超限的 413，请求没到飞书，刷新令牌还能用）都算临时故障，
    /// 授权状态不变、稍后再试。
    pub(crate) async fn refresh_feishu(
        &self,
        grant: &AuthGrant,
        now_secs: u64,
    ) -> ImResult<AuthGrant> {
        let error = |kind, message: &str| ImError {
            kind,
            message: message.to_owned(),
            platform_code: None,
            reauth: None,
            retry_after_secs: None,
        };
        let Some(refresh) = grant.refresh_token() else {
            return Err(ImError {
                reauth: Some(ReauthReason::Expired),
                ..error(ErrorKind::AuthExpired, "飞书授权没有刷新令牌，请重新登录。")
            });
        };
        let response = self
            .http
            .post(self.endpoint("/v1/feishu/refresh"))
            .json(&serde_json::json!({ "refreshToken": refresh }))
            .send()
            .await
            .map_err(|failure| {
                let cause = crate::net::network_cause(failure);
                tracing::warn!(%cause, "经中继续期飞书授权失败：连不上中继");
                error(
                    ErrorKind::Disconnected,
                    "授权回调服务暂时连不上，飞书登录稍后自动续期。",
                )
            })?;
        let status = response.status();
        if status == StatusCode::OK {
            let refreshed: Refreshed = response.json().await.map_err(|failure| {
                let cause = crate::net::network_cause(failure);
                tracing::warn!(%cause, "经中继续期飞书授权：回执读不懂");
                error(
                    ErrorKind::ProtocolError,
                    "授权回调服务的续期回执读不懂，请重新登录。",
                )
            })?;
            return refreshed
                .token
                .into_grant(now_secs)
                .map_err(|reason| error(ErrorKind::ProtocolError, &reason));
        }
        let body = response.json::<RelayFailure>().await.ok();
        let code = body.as_ref().map(|body| body.error.code.as_str());
        tracing::warn!(status = status.as_u16(), code, "经中继续期飞书授权未成功");
        match (status, body) {
            (StatusCode::BAD_REQUEST, Some(body))
                if body.error.code == "feishu_refresh_rejected" =>
            {
                Err(ImError {
                    platform_code: body.error.feishu_code.map(|code| code.to_string()),
                    reauth: Some(ReauthReason::Expired),
                    ..error(ErrorKind::AuthExpired, "飞书登录已失效，请重新登录。")
                })
            }
            (StatusCode::TOO_MANY_REQUESTS, _) => Err(error(
                ErrorKind::RateLimited,
                "授权回调服务续期太频繁，稍后自动再试。",
            )),
            _ => Err(error(
                ErrorKind::Disconnected,
                "授权回调服务暂时不能续期飞书登录，稍后自动再试。",
            )),
        }
    }
}

/// 中继给的中文说明：去掉控制字符、限制长度，防止异常响应把提示撑满。
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(RELAY_MESSAGE_MAX_CHARS)
        .collect()
}

/// 中继给的错误：读它的中文说明；404/410 表示这次登录在中继那边已不存在。日志只记编号与状态码。
async fn failure(
    step: &'static str,
    status: StatusCode,
    response: reqwest::Response,
) -> RelayError {
    let body = response.json::<RelayFailure>().await.ok();
    let code = body.as_ref().map(|body| body.error.code.as_str());
    tracing::warn!(
        platform = PLATFORM,
        step,
        status = status.as_u16(),
        code,
        "IM 授权中继拒绝了请求"
    );
    match body.as_ref().map(|body| body.error.code.as_str()) {
        // 中继没配飞书应用：直接给原因（契约 v5 F6），不是临时故障。
        Some("feishu_exchange_unavailable") => {
            return RelayError {
                message: FEISHU_UNAVAILABLE.into(),
                gone: false,
                transient: false,
            };
        }
        // 领取时中继连不上飞书：授权码已经作废，不再领取，请用户重新登录。
        Some("feishu_unreachable") => {
            return RelayError {
                message: "授权回调服务暂时连不上飞书，请重新登录。".into(),
                gone: true,
                transient: false,
            };
        }
        _ => {}
    }
    let gone = matches!(status, StatusCode::NOT_FOUND | StatusCode::GONE);
    let transient = status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS;
    let message = match body {
        Some(body) if gone => {
            tracing::debug!(code = %body.error.code, "中继说这次登录已不存在");
            "这次登录已失效，请重新登录。".to_owned()
        }
        Some(body) => format!("授权回调服务：{}", clean(&body.error.message)),
        None if gone => "这次登录已失效，请重新登录。".to_owned(),
        None => format!(
            "授权回调服务暂时不可用（HTTP {}），请稍后再试。",
            status.as_u16()
        ),
    };
    RelayError {
        message,
        gone,
        transient,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_origins_accept_https_and_loopback_http_only() {
        assert!(parse_relay_url(DEFAULT_RELAY).is_ok());
        assert!(parse_relay_url("http://127.0.0.1:18080").is_ok());
        assert!(parse_relay_url("http://localhost:18080/").is_ok());
        for raw in [
            "http://relay.example",
            "https://relay.example/path",
            "https://u:p@relay.example",
            "https://relay.example/?x=1",
            "ftp://relay.example",
            "not a url",
        ] {
            assert!(parse_relay_url(raw).is_err(), "{raw}");
        }
        assert_eq!(
            normalize_relay_url(" https://Relay.Example/ ").unwrap(),
            "https://relay.example"
        );
        assert_eq!(stored_relay_url(DEFAULT_RELAY), None);
        assert_eq!(
            stored_relay_url("https://relay.example/").as_deref(),
            Some("https://relay.example")
        );
    }

    #[test]
    fn relay_tokens_need_an_expiry_and_bearer() {
        let token = |value: serde_json::Value| serde_json::from_value::<RelayToken>(value).unwrap();
        let grant = token(serde_json::json!({
            "access_token": "u-a", "refresh_token": "ur-r", "expires_in": 7200,
            "scope": "im:message offline_access", "token_type": "Bearer",
        }))
        .into_grant(100)
        .unwrap();
        assert_eq!(grant.expires_at(), Some(7300));
        assert_eq!(grant.scopes().unwrap(), ["im:message", "offline_access"]);
        assert!(
            token(serde_json::json!({"access_token": "a", "expires_in": 0}))
                .into_grant(1)
                .is_err()
        );
        assert!(
            token(serde_json::json!({"access_token": "a", "expires_in": 9, "token_type": "mac"}))
                .into_grant(1)
                .is_err()
        );
    }

    #[test]
    fn the_callback_is_fixed_per_relay() {
        assert_eq!(
            Relay::new(DEFAULT_RELAY).unwrap().callback_url(),
            "https://hhh-im-auth.magiclizi.com/hhh-friend-im/feishu/callback"
        );
    }
}
