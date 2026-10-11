//! 飞书错误码 → 统一分类与说明（契约 hhh-im client v17 §5）：错误码记成文字；续期被拒时给出重新授权原因。

use crate::*;

/// 飞书的一个错误（业务码与 HTTP 状态）翻成统一错误；没有业务码时 `code` 为 -1。
pub(crate) fn from_code(code: i64, status: u16, retry_after_secs: Option<u64>) -> ImError {
    let (kind, message) = match code {
        231011 | 231021 => (
            ErrorKind::InvalidRequest,
            "搜索关键词或页大小超出平台限制，请缩短关键词或减少每页数量。",
        ),
        231020 => (
            ErrorKind::InvalidRequest,
            "搜索游标无效或已过期，请重新输入关键词，从第一页搜索。",
        ),
        231022 => (
            ErrorKind::LimitExceeded,
            "已达到平台搜索翻页上限，请使用更具体的关键词重新搜索。",
        ),
        // 20024／20026／20037／20064／20073：刷新令牌不属于本应用、无效、过期、已撤销或已用过。
        99991661 | 99991663 | 99991668 | 99991671 | 20004 | 20005 | 20006 | 20024 | 20026
        | 20037 | 20064 | 20073 => (ErrorKind::AuthExpired, "飞书登录无效或已过期，请重新登录。"),
        20074 => (
            ErrorKind::PermissionDenied,
            "飞书应用没有开启刷新授权，请在开发者后台的安全设置中开启刷新 user_access_token 并发布新版本，再重新登录。",
        ),
        20002 => (
            ErrorKind::InvalidRequest,
            "飞书 App Secret 无效，请更新应用配置后重新登录。",
        ),
        99991672 | 99991679 | 99991680 | 230006 | 230027 | 231008 | 20010 | 20027 | 20068 => (
            ErrorKind::PermissionDenied,
            "当前应用或用户缺少权限，请检查飞书权限与会话访问范围。",
        ),
        99991400 | 99991401 | 230020 => (ErrorKind::RateLimited, "飞书请求过于频繁，请稍后重试。"),
        // 撤回：超出企业设置的撤回时限、只能撤回自己的、看不到、不支持的类型、已删除。
        230009 => (ErrorKind::LimitExceeded, "消息发出时间过长，不能撤回。"),
        230026 => (ErrorKind::PermissionDenied, "只能撤回自己发出的消息。"),
        230050 => (
            ErrorKind::PermissionDenied,
            "当前账号看不到这条消息，无法撤回。",
        ),
        230054 => (ErrorKind::Unsupported, "这类消息不支持撤回。"),
        230110 => (ErrorKind::NotFound, "消息已被删除，无法撤回。"),
        230011 | 230001 | 232001 | 231003 => (
            ErrorKind::NotFound,
            "会话或消息不存在，或当前账号不可访问。",
        ),
        _ if status == 401 => (ErrorKind::AuthExpired, "飞书登录无效，请重新登录。"),
        _ if status == 403 => (ErrorKind::PermissionDenied, "当前账号无权执行该操作。"),
        _ if status == 429 => (ErrorKind::RateLimited, "飞书请求过于频繁，请稍后重试。"),
        _ if status == 404 => (ErrorKind::NotFound, "指定资源不存在或不可访问。"),
        _ if status >= 500 => (ErrorKind::Disconnected, "飞书服务暂不可用，请稍后重试。"),
        _ => (
            ErrorKind::ProtocolError,
            "飞书拒绝了请求，请按错误码检查参数、应用状态与权限。",
        ),
    };
    let mut error = ImError::new(kind, message);
    error.platform_code = Some(code.to_string());
    error.reauth = reauth_of(code, kind);
    error.retry_after_secs = retry_after_secs;
    error
}

/// 续期被拒时要用户做什么：20074 是应用没开启刷新授权，20002 是 App Secret 无效，其余授权失败是过期。
fn reauth_of(code: i64, kind: ErrorKind) -> Option<ReauthReason> {
    match code {
        20074 => Some(ReauthReason::RefreshDisabled),
        20002 => Some(ReauthReason::AppCredentialInvalid),
        _ if kind == ErrorKind::AuthExpired => Some(ReauthReason::Expired),
        _ => None,
    }
}

/// 统一错误里记着的飞书业务码。
pub(crate) fn code_of(error: &ImError) -> Option<i64> {
    error.platform_code.as_deref()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // 续期被拒的三种原因分得清，临时故障不要用户重新授权；错误码记成文字、读回还是原来的数。
    #[test]
    fn refusals_say_what_the_user_must_do_and_codes_are_kept_as_text() {
        let disabled = from_code(20074, 400, None);
        assert_eq!(disabled.reauth, Some(ReauthReason::RefreshDisabled));
        assert_eq!(disabled.platform_code.as_deref(), Some("20074"));
        assert_eq!(code_of(&disabled), Some(20074));
        assert_eq!(
            from_code(20002, 400, None).reauth,
            Some(ReauthReason::AppCredentialInvalid)
        );
        let expired = from_code(20037, 400, None);
        assert_eq!(expired.kind, ErrorKind::AuthExpired);
        assert_eq!(expired.reauth, Some(ReauthReason::Expired));
        assert_eq!(from_code(-1, 401, None).reauth, Some(ReauthReason::Expired));
        for transient in [from_code(99991400, 429, Some(3)), from_code(-1, 503, None)] {
            assert_eq!(transient.reauth, None, "{}", transient.message);
        }
        assert_eq!(
            from_code(230026, 400, None).reauth,
            None,
            "普通拒绝不要求重新授权"
        );
        assert_eq!(
            from_code(-1, 500, None).to_string(),
            "飞书服务暂不可用，请稍后重试。（错误码：-1）"
        );
    }
}
