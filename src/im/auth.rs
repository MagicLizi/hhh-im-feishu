//! 用户 OAuth：只封装授权请求与令牌交换，凭据保存归宿主。

use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::Url;
use serde_json::Value;

use crate::{ErrorKind, ImError, ImResult};

/// 授权交换允许等待五分钟，与一次性授权码有效期一致。
pub(crate) const LOGIN_TTL_SECS: u64 = 300;
/// 给平台长令牌保留空间，同时防止不可信凭据占用无界内存。
const TOKEN_MAX_BYTES: usize = 16 * 1024;
/// 官方授权页一次最多申请 200 项权限。
pub(crate) const OAUTH_SCOPE_MAX: usize = 200;

pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// 运行时用户授权；不实现通用 JSON 序列化，Debug 永不显示令牌。
#[derive(Clone)]
pub struct AuthGrant {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<u64>,
    scopes: Option<Vec<String>>,
    /// 实时推送用的应用级令牌（Slack 的 `xapp-`，契约 client v11 §11 第 2 项）：只能由用户在平台后台生成，
    /// 宿主随授权一起给；平台不需要时为空。
    app_token: Option<String>,
}
impl std::fmt::Debug for AuthGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthGrant")
            .field("credentials", &"[已隐藏]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}
impl AuthGrant {
    /// 注入已取得的用户访问令牌；授权范围保持未知。
    pub fn from_access_token(token: impl Into<String>) -> ImResult<Self> {
        Self::from_parts(token.into(), None, None, None)
    }

    /// 由宿主从自己的安全凭据存储恢复授权。
    pub fn from_parts(
        access_token: String,
        refresh_token: Option<String>,
        expires_at: Option<u64>,
        scopes: Option<Vec<String>>,
    ) -> ImResult<Self> {
        for token in std::iter::once(&access_token).chain(refresh_token.iter()) {
            if token.is_empty()
                || token.len() > TOKEN_MAX_BYTES
                || token.chars().any(char::is_whitespace)
                || token.chars().any(char::is_control)
            {
                return Err(ImError::new(
                    ErrorKind::InvalidRequest,
                    "授权令牌为空、过长或包含非法字符。",
                ));
            }
        }
        Ok(Self {
            access_token,
            refresh_token,
            expires_at,
            scopes,
            app_token: None,
        })
    }

    /// 带上实时推送用的应用级令牌（Slack 的 `xapp-`，契约 client v11 §11 第 2 项）；hhh-im 不保存它，
    /// 也不写进日志。
    ///
    /// # Errors
    /// 令牌为空、过长或包含空白与控制字符时返回 [`ErrorKind::InvalidRequest`]。
    ///
    /// # Examples
    ///
    /// ```
    /// let grant = hhh_im_feishu::AuthGrant::from_access_token("xoxp-1").unwrap().with_app_token("xapp-1").unwrap();
    /// assert_eq!(grant.app_token(), Some("xapp-1"));
    /// assert!(!format!("{grant:?}").contains("xapp-1"));
    /// ```
    pub fn with_app_token(mut self, token: impl Into<String>) -> ImResult<Self> {
        let token = token.into();
        if token.is_empty()
            || token.len() > TOKEN_MAX_BYTES
            || token.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "应用级令牌为空、过长或包含非法字符。",
            ));
        }
        self.app_token = Some(token);
        Ok(self)
    }

    /// 实时推送用的应用级令牌；宿主没给时为空。
    pub fn app_token(&self) -> Option<&str> {
        self.app_token.as_deref()
    }

    /// 供请求签发或宿主安全保存使用的敏感令牌。
    pub fn access_token(&self) -> &str {
        &self.access_token
    }
    /// 可轮换的敏感刷新令牌；可能未授予 offline_access。
    pub fn refresh_token(&self) -> Option<&str> {
        self.refresh_token.as_deref()
    }
    /// 服务返回的访问令牌过期时刻，单位为 Unix 秒。
    pub fn expires_at(&self) -> Option<u64> {
        self.expires_at
    }
    /// 服务实际授予的权限，未知时为 None。
    pub fn scopes(&self) -> Option<&[String]> {
        self.scopes.as_deref()
    }

    pub(crate) fn ensure_current(&self) -> ImResult<()> {
        if self.expires_at.is_some_and(|expires| expires <= now_secs()) {
            return Err(ImError::new(
                ErrorKind::AuthExpired,
                "用户授权已过期，请刷新或重新登录。",
            ));
        }
        Ok(())
    }

    pub(crate) fn from_response(value: Value) -> ImResult<Self> {
        let token = value
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| ImError::new(ErrorKind::ProtocolError, "授权响应缺少访问令牌。"))?;
        let expires = value
            .get("expires_in")
            .and_then(Value::as_u64)
            .filter(|v| *v > 0)
            .ok_or_else(|| {
                ImError::new(ErrorKind::ProtocolError, "授权响应缺少有效的过期时间。")
            })?;
        if value
            .get("token_type")
            .and_then(Value::as_str)
            .is_some_and(|v| !v.eq_ignore_ascii_case("bearer"))
        {
            return Err(ImError::new(
                ErrorKind::ProtocolError,
                "授权响应的令牌类型不受支持。",
            ));
        }
        Self::from_parts(
            token.to_owned(),
            value
                .get("refresh_token")
                .and_then(Value::as_str)
                .map(str::to_owned),
            Some(now_secs().saturating_add(expires)),
            value
                .get("scope")
                .and_then(Value::as_str)
                .map(|s| s.split_whitespace().map(str::to_owned).collect()),
        )
    }
}

/// 本次登录需要的回调和权限。
#[derive(Debug, Clone)]
pub struct LoginRequest {
    /// 已在应用后台登记的回调 URL。
    pub redirect_uri: String,
    /// 本次申请的用户权限。
    pub scopes: Vec<String>,
}

/// 一次性登录上下文；以值传递消费，不能复制后重复换码。
pub struct PendingLogin {
    pub(crate) url: Url,
    pub(crate) redirect: Url,
    pub(crate) state: String,
    pub(crate) created_at: u64,
}
impl PendingLogin {
    /// 交给用户浏览器打开的授权地址。
    pub fn authorization_url(&self) -> &str {
        self.url.as_str()
    }

    /// 本次登录的一次性 state（已写在授权地址里）。宿主经公网回调中继领取结果时，
    /// 先用它在中继登记，再打开授权页；核对仍由 `finish_login` 做。
    ///
    /// # Examples
    ///
    /// ```
    /// let auth = hhh_im_feishu::FeishuAuth::new(hhh_im_feishu::FeishuConfig::default(), "app".into(), "secret".into()).unwrap();
    /// let pending = auth
    ///     .begin_login(hhh_im_feishu::LoginRequest {
    ///         redirect_uri: "https://relay.example/hhh-friend-im/feishu/callback".into(),
    ///         scopes: vec![],
    ///     })
    ///     .unwrap();
    /// assert!(pending.authorization_url().contains(&format!("state={}", pending.state())));
    /// ```
    pub fn state(&self) -> &str {
        &self.state
    }
}
impl std::fmt::Debug for PendingLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PendingLogin([一次性状态已隐藏])")
    }
}

/// 生成可用于操作编号或授权状态的随机字符串。
pub fn random_id() -> ImResult<String> {
    let mut bytes = [0_u8; 24];
    getrandom::fill(&mut bytes)
        .map_err(|_| ImError::new(ErrorKind::ProtocolError, "无法取得系统随机数。"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn credentials_are_redacted_and_expiry_is_checked() {
        let grant = AuthGrant::from_parts(
            "private_access".into(),
            Some("private_refresh".into()),
            Some(1),
            None,
        )
        .unwrap();
        let debug = format!("{grant:?}");
        assert!(!debug.contains("private"));
        assert_eq!(
            grant.ensure_current().unwrap_err().kind,
            ErrorKind::AuthExpired
        );
        for token in ["", "a b", "a\r\nb"] {
            assert!(AuthGrant::from_access_token(token).is_err());
        }
        assert!(AuthGrant::from_access_token("x".repeat(TOKEN_MAX_BYTES + 1)).is_err());
    }

    #[test]
    fn token_response_uses_actual_expiry_and_rotated_grant() {
        let before = now_secs();
        let grant = AuthGrant::from_response(json!({"access_token":"new", "refresh_token":"rotated", "expires_in":77, "scope":"im:message offline_access", "token_type":"Bearer"})).unwrap();
        assert_eq!(grant.refresh_token(), Some("rotated"));
        assert!(grant.expires_at().unwrap() >= before + 77);
        assert!(grant.expires_at().unwrap() <= now_secs() + 77);
        assert_eq!(grant.scopes().unwrap().len(), 2);
        assert!(AuthGrant::from_response(json!({"access_token":"x", "expires_in":0})).is_err());
        assert!(
            AuthGrant::from_response(
                json!({"access_token":"x", "expires_in":5, "token_type":"Basic"})
            )
            .is_err()
        );
    }
}
