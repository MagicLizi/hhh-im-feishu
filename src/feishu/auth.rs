//! 飞书用户授权（OAuth v3）：登录、换码与续期；应用密钥由调用方注入，库不保存凭据。

use reqwest::Url;
use serde_json::{Value, json};

use super::{http_client, read_json};
use crate::im::auth::{LOGIN_TTL_SECS, OAUTH_SCOPE_MAX, now_secs};
use crate::{
    AuthGrant, ErrorKind, FeishuConfig, ImError, ImResult, LoginRequest, PendingLogin, random_id,
};

/// 飞书用户授权接口，应用密钥由调用方注入。
pub struct FeishuAuth {
    config: FeishuConfig,
    app_id: String,
    app_secret: String,
    http: reqwest::Client,
}
impl std::fmt::Debug for FeishuAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeishuAuth")
            .field("app_id", &self.app_id)
            .finish_non_exhaustive()
    }
}
impl FeishuAuth {
    /// 装配自建应用的用户授权客户端。
    pub fn new(config: FeishuConfig, app_id: String, app_secret: String) -> ImResult<Self> {
        config.validate()?;
        if app_id.trim().is_empty() || app_secret.trim().is_empty() {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "请提供飞书应用的 App ID 和 App Secret。",
            ));
        }
        Ok(Self {
            config,
            app_id,
            app_secret,
            http: http_client()?,
        })
    }

    /// 只用来拼授权页的入口：只有 App ID，App Secret 在别处（例如替客户端换令牌的回调中继）。
    /// 用它 [`Self::finish_login`] / [`Self::refresh_auth`] 会直接报错，令牌交给持有 Secret 的一方去换。
    ///
    /// # Examples
    ///
    /// ```
    /// let auth = hhh_im_feishu::FeishuAuth::authorize_only(hhh_im_feishu::FeishuConfig::default(), "cli_relay".into()).unwrap();
    /// let state = hhh_im_feishu::random_id().unwrap();
    /// let pending = auth
    ///     .begin_login_with_state(
    ///         hhh_im_feishu::LoginRequest {
    ///             redirect_uri: "https://relay.example/hhh-friend-im/feishu/callback".into(),
    ///             scopes: vec!["offline_access".into()],
    ///         },
    ///         state.clone(),
    ///     )
    ///     .unwrap();
    /// assert_eq!(pending.state(), state);
    /// assert!(pending.authorization_url().contains("client_id=cli_relay"));
    /// ```
    pub fn authorize_only(config: FeishuConfig, app_id: String) -> ImResult<Self> {
        config.validate()?;
        if app_id.trim().is_empty() {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "请提供飞书应用的 App ID。",
            ));
        }
        Ok(Self {
            config,
            app_id,
            app_secret: String::new(),
            http: http_client()?,
        })
    }

    /// 没有 App Secret（[`Self::authorize_only`]）时换不了令牌。
    fn require_secret(&self) -> ImResult<()> {
        if self.app_secret.is_empty() {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "这个飞书应用的 App Secret 不在本机，换令牌要交给持有它的服务。",
            ));
        }
        Ok(())
    }

    /// 构造官方授权页，生成随机 state；浏览器与回调监听由宿主负责。
    pub fn begin_login(&self, request: LoginRequest) -> ImResult<PendingLogin> {
        self.begin_login_with_state(request, random_id()?)
    }

    /// 同 [`Self::begin_login`]，state 由调用方给（宿主要先拿 state 去回调中继登记、拿到中继的 App ID
    /// 才能拼授权页时用）。state 要是 32–128 位随机的 URL 安全字符。
    pub fn begin_login_with_state(
        &self,
        request: LoginRequest,
        state: String,
    ) -> ImResult<PendingLogin> {
        if !(32..=128).contains(&state.len())
            || !state
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "授权 state 必须是 32–128 位随机的 URL 安全字符。",
            ));
        }
        let redirect = Url::parse(&request.redirect_uri)
            .map_err(|_| ImError::new(ErrorKind::InvalidRequest, "授权回调地址格式不正确。"))?;
        super::validate_url(&redirect)?;
        if redirect.query().is_some() || redirect.fragment().is_some() {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "回调地址不能预先包含查询参数或片段。",
            ));
        }
        if request.scopes.len() > OAUTH_SCOPE_MAX
            || request
                .scopes
                .iter()
                .any(|s| s.is_empty() || s.chars().any(char::is_whitespace))
        {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "授权权限格式不正确或超过平台上限。",
            ));
        }
        let mut scopes = request.scopes;
        scopes.sort();
        scopes.dedup();
        let mut url = Url::parse(&self.config.authorize_url)
            .map_err(|_| ImError::new(ErrorKind::InvalidRequest, "授权端点不正确。"))?;
        url.query_pairs_mut()
            .append_pair("client_id", &self.app_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", redirect.as_str())
            .append_pair("scope", &scopes.join(" "))
            .append_pair("state", &state);
        Ok(PendingLogin {
            url,
            redirect,
            state,
            created_at: now_secs(),
        })
    }

    /// 校验完整回调地址并交换用户令牌，不自动重试一次性授权码。
    pub async fn finish_login(
        &self,
        pending: PendingLogin,
        callback_url: &str,
    ) -> ImResult<AuthGrant> {
        self.require_secret()?;
        if now_secs().saturating_sub(pending.created_at) > LOGIN_TTL_SECS {
            return Err(ImError::new(
                ErrorKind::AuthExpired,
                "本次授权等待已超时，请重新发起登录。",
            ));
        }
        let callback = Url::parse(callback_url)
            .map_err(|_| ImError::new(ErrorKind::InvalidRequest, "授权回调地址无效。"))?;
        let mut target = callback.clone();
        target.set_query(None);
        if target != pending.redirect {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "授权回调地址与本次登录不匹配。",
            ));
        }
        let fields: Vec<_> = callback.query_pairs().collect();
        let single = |key: &str| -> ImResult<Option<String>> {
            let values: Vec<_> = fields.iter().filter(|(k, _)| k == key).collect();
            if values.len() > 1 {
                return Err(ImError::new(
                    ErrorKind::InvalidRequest,
                    "授权回调包含重复参数。",
                ));
            }
            Ok(values.first().map(|(_, value)| value.to_string()))
        };
        if single("state")?.as_deref() != Some(&pending.state) {
            return Err(ImError::new(
                ErrorKind::PermissionDenied,
                "授权回调的 state 不匹配，已拒绝换取令牌。",
            ));
        }
        if single("error")?.is_some() {
            return Err(ImError::new(ErrorKind::Cancelled, "用户未完成飞书授权。"));
        }
        let code = single("code")?
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ImError::new(ErrorKind::InvalidRequest, "授权回调缺少授权码。"))?;
        self.exchange(json!({"grant_type":"authorization_code","client_id":self.app_id,"client_secret":self.app_secret,"code":code,"redirect_uri":pending.redirect.as_str()})).await
    }

    /// 换取并返回新凭据；宿主应先安全保存，再更新账号客户端。
    pub async fn refresh_auth(&self, grant: &AuthGrant) -> ImResult<AuthGrant> {
        self.require_secret()?;
        let refresh = grant.refresh_token().ok_or_else(|| {
            ImError::new(
                ErrorKind::AuthExpired,
                "本次授权没有刷新令牌，请重新登录并申请 offline_access。",
            )
        })?;
        self.exchange(json!({"grant_type":"refresh_token","client_id":self.app_id,"client_secret":self.app_secret,"refresh_token":refresh})).await
    }

    async fn exchange(&self, body: Value) -> ImResult<AuthGrant> {
        // 一次性授权码和刷新令牌不能因普通网络重试而重复消费。
        let response = self
            .http
            .post(&self.config.token_url)
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                ImError::network(
                    error,
                    "授权请求未得到可靠响应，请重新登录；刷新令牌可能已经轮换。",
                )
            })?;
        AuthGrant::from_response(read_json(response).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn expired_login_and_missing_refresh_fail_without_network() {
        let auth = FeishuAuth::new(FeishuConfig::default(), "app".into(), "secret".into()).unwrap();
        let mut pending = auth
            .begin_login(LoginRequest {
                redirect_uri: "http://127.0.0.1:1234/callback".into(),
                scopes: vec![],
            })
            .unwrap();
        pending.created_at = now_secs() - LOGIN_TTL_SECS - 1;
        assert_eq!(
            auth.finish_login(pending, "http://127.0.0.1:1234/callback")
                .await
                .unwrap_err()
                .kind,
            ErrorKind::AuthExpired
        );
        assert_eq!(
            auth.refresh_auth(&AuthGrant::from_access_token("a").unwrap())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::AuthExpired
        );
    }

    #[tokio::test]
    async fn authorize_only_builds_the_page_with_the_given_state_but_never_exchanges() {
        let auth = FeishuAuth::authorize_only(FeishuConfig::default(), "cli_relay".into()).unwrap();
        let request = || LoginRequest {
            redirect_uri: "https://relay.example/hhh-friend-im/feishu/callback".into(),
            scopes: vec!["offline_access".into()],
        };
        let state = "s".repeat(48);
        let pending = auth
            .begin_login_with_state(request(), state.clone())
            .unwrap();
        assert_eq!(pending.state(), state);
        let url = Url::parse(pending.authorization_url()).unwrap();
        let pairs: Vec<_> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert!(pairs.contains(&("client_id".into(), "cli_relay".into())));
        assert!(pairs.contains(&("state".into(), state.clone())));
        for bad in [
            "short".to_owned(),
            "s".repeat(129),
            format!("{}!", "s".repeat(40)),
        ] {
            assert_eq!(
                auth.begin_login_with_state(request(), bad)
                    .unwrap_err()
                    .kind,
                ErrorKind::InvalidRequest
            );
        }
        let callback =
            format!("https://relay.example/hhh-friend-im/feishu/callback?state={state}&code=c");
        assert_eq!(
            auth.finish_login(pending, &callback)
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidRequest
        );
        let grant = AuthGrant::from_parts("a".into(), Some("r".into()), None, None).unwrap();
        assert_eq!(
            auth.refresh_auth(&grant).await.unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
        assert!(FeishuAuth::authorize_only(FeishuConfig::default(), " ".into()).is_err());
    }
}
