//! 上传要发送的图片与文件：用本人令牌上传，拿到平台资源编号后再以本人身份发送。
//!
//! 上传与发送用同一种身份：平台只允许发送自己上传的资源。

use super::*;

/// 平台图片上限 10 MB，且不能为空。
pub const IMAGE_UPLOAD_MAX_BYTES: usize = 10 * 1024 * 1024;
/// 平台文件上限 30 MB，且不能为空。
pub const FILE_UPLOAD_MAX_BYTES: usize = 30 * 1024 * 1024;
/// 文件名在请求里原样带上；超长的名字按损坏处理，不截断改名。
const UPLOAD_NAME_MAX_BYTES: usize = 250;

/// 平台文件种类：音频与常见文档有专名，其余一律按普通文件上传。
/// 视频（mp4）也按普通文件上传：附件一律作为文件消息发出，按 `mp4` 上传的文件飞书只认视频消息，发文件消息会被拒
/// （真实环境 2026-10-09：错误码 230055，上传的文件类型与消息类型不符；契约 friend/video-preview v1 发送不变）。
fn file_type(name: &str) -> &'static str {
    let extension = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "opus" => "opus",
        "pdf" => "pdf",
        "doc" | "docx" => "doc",
        "xls" | "xlsx" => "xls",
        "ppt" | "pptx" => "ppt",
        _ => "stream",
    }
}

impl FeishuClient {
    /// 上传一个要发送的图片或文件，返回平台资源编号。
    ///
    /// 图片最大 10 MB、文件最大 30 MB，都不能为空；需要 `im:resource` 权限，
    /// 已有授权缺这项时需要重新授权。
    ///
    /// # Errors
    /// 内容为空或超过上限、文件名无效、缺少权限或平台拒绝时返回错误；上传不自动重试。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example(client: &hhh_im_feishu::FeishuClient) -> hhh_im_feishu::ImResult<()> {
    /// let key = client
    ///     .upload_resource(hhh_im_feishu::ResourceKind::Image, "截图.png", vec![0x89, b'P', b'N', b'G'])
    ///     .await?;
    /// let content = vec![hhh_im_feishu::OutgoingContent::Image { key }];
    /// # let _ = content;
    /// # Ok(()) }
    /// ```
    pub async fn upload_resource(
        &self,
        kind: ResourceKind,
        name: &str,
        bytes: Vec<u8>,
    ) -> ImResult<String> {
        self.grant.ensure_current()?;
        let max = match kind {
            ResourceKind::Image => IMAGE_UPLOAD_MAX_BYTES,
            ResourceKind::File => FILE_UPLOAD_MAX_BYTES,
            // 飞书只允许转发收到过的表情包，没有上传表情包的接口（契约 client v15）。
            ResourceKind::Sticker => {
                return Err(ImError::new(ErrorKind::Unsupported, "表情包不能上传。"));
            }
        };
        if bytes.is_empty() || bytes.len() > max {
            return Err(ImError::new(
                ErrorKind::LimitExceeded,
                "图片须在 10 MB 以内、文件须在 30 MB 以内，且不能为空。",
            ));
        }
        if name.trim().is_empty()
            || name.len() > UPLOAD_NAME_MAX_BYTES
            || name
                .chars()
                .any(|c| c.is_control() || c == '/' || c == '\\')
        {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "附件文件名为空、过长或含有路径符号。",
            ));
        }
        if self.lacks_scope("im:resource") && self.lacks_scope("im:resource:upload") {
            return Err(ImError::new(
                ErrorKind::PermissionDenied,
                "发送图片和文件需要补充飞书权限，请重新登录后再试。",
            ));
        }
        let size = bytes.len();
        // multipart（表单分段）：一段是参数，一段是文件字节；文件字节整体交给请求，不在本库另存。
        let part = reqwest::multipart::Part::bytes(bytes).file_name(name.to_owned());
        let (path, form, key_field) = match kind {
            // 表情包在上面已经拒绝。
            ResourceKind::Image | ResourceKind::Sticker => (
                "images",
                reqwest::multipart::Form::new()
                    .text("image_type", "message")
                    .part("image", part),
                "image_key",
            ),
            ResourceKind::File => (
                "files",
                reqwest::multipart::Form::new()
                    .text("file_type", file_type(name))
                    .text("file_name", name.to_owned())
                    .part("file", part),
                "file_key",
            ),
        };
        let data = self
            .query_data(
                self.http
                    .post(self.url(&["open-apis", "im", "v1", path])?)
                    .multipart(form),
            )
            .await?;
        let key = required(&data, key_field)?;
        validate_id(&key)?;
        tracing::info!(?kind, bytes = size, "飞书已接收附件上传");
        Ok(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_keep_their_platform_type_and_others_upload_as_stream() {
        assert_eq!(file_type("报告.PDF"), "pdf");
        assert_eq!(file_type("回归用例.xlsx"), "xls");
        assert_eq!(file_type("slides.pptx"), "ppt");
        // 附件按文件消息发：mp4 按视频类型上传会被飞书拒（230055）。
        assert_eq!(file_type("clip.mp4"), "stream");
        assert_eq!(file_type("录屏.MP4"), "stream");
        assert_eq!(file_type("notes.txt"), "stream");
        assert_eq!(file_type("没有扩展名"), "stream");
    }
}
