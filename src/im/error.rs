//! 调用方可据以处理的错误，不保留可能回显密钥的响应原文。

use serde::{Deserialize, Serialize};

/// 接口失败的原因分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorKind {
    /// 平台或当前接入方式不提供该能力。
    Unsupported,
    /// 当前账号或应用缺少权限。
    PermissionDenied,
    /// 用户授权失效或过期。
    AuthExpired,
    /// 达到平台调用频率限制。
    RateLimited,
    /// 指定对象不存在或不可见。
    NotFound,
    /// 调用参数不合法。
    InvalidRequest,
    /// 网络中断或超时。
    Disconnected,
    /// 响应不符合已知协议。
    ProtocolError,
    /// 用户取消了操作。
    Cancelled,
    /// 响应或文件超过资源预算。
    LimitExceeded,
    /// 本地消息库读写失败，或库文件格式不受支持。
    Storage,
}

/// 授权失败时要用户做什么（契约 hhh-im client v17 §5）：宿主据此提示，不认平台的错误码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReauthReason {
    /// 授权过期或失效：重新授权即可。
    Expired,
    /// 平台关闭了续期（例如应用没开启刷新授权）：先在平台后台打开，再重新授权。
    RefreshDisabled,
    /// 应用凭据无效（例如应用密钥错了）：先更新应用配置，再重新授权。
    AppCredentialInvalid,
}

/// 脱敏后的接口错误。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImError {
    /// 可由程序匹配的分类。
    pub kind: ErrorKind,
    /// 面向用户的中文说明，不包含平台原始响应。
    pub message: String,
    /// 平台原样的错误码，记成文字（飞书的数字码转成文字，Slack 的错误名原样；契约 hhh-im client v17 §5）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_code: Option<String>,
    /// 授权失败时要用户做什么：由平台适配器按平台的错误给出；临时故障为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reauth: Option<ReauthReason>,
    /// 平台建议等待的秒数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
}

impl ImError {
    pub(crate) fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        let message = message.into();
        tracing::warn!(?kind, message, "IM 操作未完成");
        Self {
            kind,
            message,
            platform_code: None,
            reauth: None,
            retry_after_secs: None,
        }
    }

    /// 网络请求失败（连接、TLS 握手、超时、读取中断）：给用户的说明不变，日志里写明底层原因。
    ///
    /// 只丢一句“连不上”排查不了：2026-09-28 飞书握手被网络设备重置，日志里只剩
    /// “无法连接飞书”，要另写探针才看到 `os error 10054`（契约 hhh-im client v5 §4）。
    pub(crate) fn network(error: reqwest::Error, message: impl Into<String>) -> Self {
        let message = message.into();
        let cause = crate::net::network_cause(error);
        tracing::warn!(message, %cause, "IM 网络请求失败");
        Self {
            kind: ErrorKind::Disconnected,
            message,
            platform_code: None,
            reauth: None,
            retry_after_secs: None,
        }
    }
}

impl std::fmt::Display for ImError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(code) = &self.platform_code {
            write!(f, "（错误码：{code}）")?;
        }
        Ok(())
    }
}
impl std::error::Error for ImError {}

/// IM 方法的统一结果。
pub type ImResult<T> = Result<T, ImError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// 本机一个刚关掉的端口：得到真实的“连接被拒绝”，给用户的说明原样保留、归为断网。
    /// 原因链与去掉地址由 `crate::net::network_cause` 的单测锁住。
    #[tokio::test]
    async fn network_errors_become_disconnected_with_the_given_message() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let wrapped = ImError::network(
            crate::net::client_builder()
                .build()
                .unwrap()
                .get(format!("http://127.0.0.1:{port}/"))
                .send()
                .await
                .expect_err("端口已关闭"),
            "无法连接飞书以核验用户身份。",
        );
        assert_eq!(wrapped.kind, ErrorKind::Disconnected);
        assert_eq!(wrapped.message, "无法连接飞书以核验用户身份。");
    }
}
