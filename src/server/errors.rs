//! 适配器的错误 → 协议的错误（protocol-v1 §12）：分类一一对应，给用户看的说明、平台错误码、等多久照原样带上。

use hhh_im_protocol::rpc::{ErrorKind as WireKind, ReauthReason as WireReauth, RpcError, code};

use crate::{ErrorKind, ImError, ReauthReason};

/// 统一错误 → 协议错误。
pub(crate) fn rpc(error: &ImError) -> RpcError {
    let mut wire = RpcError::from_kind(kind(error.kind), error.message.clone());
    if let Some(data) = wire.data.as_mut() {
        data.platform_code = error.platform_code.clone();
        data.reauth = error.reauth.map(reauth);
        data.retry_after_secs = error.retry_after_secs;
    }
    wire
}

/// 错误分类。本地库读写（`Storage`）在插件里不会出现，万一有就当成看不懂的数据。
pub(crate) fn kind(kind: ErrorKind) -> WireKind {
    match kind {
        ErrorKind::Unsupported => WireKind::Unsupported,
        ErrorKind::PermissionDenied => WireKind::PermissionDenied,
        ErrorKind::AuthExpired => WireKind::AuthExpired,
        ErrorKind::RateLimited => WireKind::RateLimited,
        ErrorKind::NotFound => WireKind::NotFound,
        ErrorKind::InvalidRequest => WireKind::InvalidRequest,
        ErrorKind::Disconnected => WireKind::Disconnected,
        ErrorKind::ProtocolError | ErrorKind::Storage => WireKind::ProtocolError,
        ErrorKind::Cancelled => WireKind::Cancelled,
        ErrorKind::LimitExceeded => WireKind::LimitExceeded,
    }
}

/// 重新授权的原因。协议没有“平台关闭了续期”这一类，记成过期（给用户看的说明里写清要做什么）。
pub(crate) fn reauth(reason: ReauthReason) -> WireReauth {
    match reason {
        ReauthReason::Expired | ReauthReason::RefreshDisabled => WireReauth::Expired,
        ReauthReason::AppCredentialInvalid => WireReauth::CredentialInvalid,
    }
}

/// 参数不合法（-32602）：宿主发来的参数读不懂。
pub(crate) fn invalid_params(error: impl std::fmt::Display) -> RpcError {
    RpcError {
        code: code::INVALID_PARAMS,
        ..RpcError::from_kind(WireKind::InvalidRequest, format!("参数不对：{error}"))
    }
}

/// 方法不存在（-32601）。
pub(crate) fn method_not_found(method: &str) -> RpcError {
    RpcError {
        code: code::METHOD_NOT_FOUND,
        ..RpcError::from_kind(WireKind::Unsupported, format!("飞书插件不支持 {method}。"))
    }
}

/// 一个给定分类与说明的协议错误。
pub(crate) fn of(kind: ErrorKind, message: impl Into<String>) -> RpcError {
    rpc(&ImError {
        kind,
        message: message.into(),
        platform_code: None,
        reauth: None,
        retry_after_secs: None,
    })
}
