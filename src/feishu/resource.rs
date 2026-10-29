//! 飞书消息资源与头像的读取（契约 hhh-im client v10 §9）：只管打开与错误分级，
//! 排队、超时、重试与存放都在下载层。
//!
//! 真实取样（2026-09-29，只读，见 friend/im-files 回溯 v1 §1.2 与验收取样记录）：
//! - 文件：200 带 `Content-Length`；`Content-Disposition: attachment; filename="<上传时的文件名>"`（非 ASCII
//!   原样按 UTF-8 给），`inner_file_data_meta` 是 JSON（`Mime`、`FileName`、`CreateTime`、`FileSize`、
//!   `IsShardingFile`）。**文件名不是通用的内容指纹**：取样的一个音频文件恰好以自己内容的 SHA-256 命名，
//!   经开放平台接口上传的文件就是原文件名（im-files v1 验收实测）；只有文件名本身是 64 位十六进制时才当
//!   指纹用。转发文件、图片都会得到新的编号（同一次验收实测），编号相同才能认出是同一个资源。
//!   `Range` 可用（4.3 MB 文件）：206 带
//!   `Content-Range: bytes 起-止/总数`，要的比文件长时给到末尾；起点超过末尾 400 + 234046。
//! - 图片：200，只有 `Content-Length`，没有内容指纹。带 `Range` 时状态是 206 却给整份、
//!   没有 `Content-Range`——图片只整份读。
//! - 错误：编号不对或不在这条消息里 400 + 234003；消息编号不存在 400 + 99992354。
//! - 头像：飞书 CDN（`https://s1-imfile.feishucdn.com/static-resource/v1/<v3_…>~?image_size=72x72&…`），
//!   不带令牌可读；换 `image_size` 得到别的尺寸。
//!
//! 表情包（2026-09-30 取样，契约 client v15 §1，friend/im-watch 回溯 v59 §2）：资源接口对表情包的 `file_key`
//! 三种读法都是 401 + 234002（文档“暂不支持获取表情包资源”）；同一编号从头像同一个 CDN
//! `…/static-resource/v1/<file_key>~` 不带令牌 4/4 读到原图（JPEG、PNG、GIF、动图 GIF），伪造编号 404。
//! **这不是开放接口写明的途径**，读不到时宿主退回“暂不支持”。

use super::{ResourceDownload, read_json, validate_id, validate_url};
use crate::*;
use reqwest::{Response, Url, header};
use serde_json::Value;

/// 飞书“获取消息中的资源文件”每秒最多调几次（契约 hhh-im client v10 §4，平台文档）；下载层限速用。
pub(crate) const RESOURCE_RATE_PER_SECOND: u32 = 50;
/// 飞书“获取消息中的资源文件”每分钟最多调几次。
pub(crate) const RESOURCE_RATE_PER_MINUTE: u32 = 1000;
/// 文件每段最多读多少字节：32 MiB。平台文档要求 100 MB 以上的文件分段、每段不超过 32 MB；
/// 取样证实小文件也支持分段，所有文件都按这个分，断了能从已收到的地方接着下。
pub const RESOURCE_SEGMENT_BYTES: u64 = 32 * 1024 * 1024;
/// 头像所在的飞书 CDN（与表情图同一批域名）。
const AVATAR_HOST_SUFFIXES: &[&str] = &[".feishucdn.com", ".pstatp.com"];
/// 飞书头像编号在地址路径里的开头。
const AVATAR_ID_PREFIX: &str = "v3_";
/// 头像地址里表示尺寸的参数。
const AVATAR_SIZE_PARAM: &str = "image_size";
/// 表情包所在的飞书图片 CDN（与头像同一个）；编号拼在后面、末尾带 `~`，不带参数就是原图（动图保留）。
const STICKER_CDN: &str = "https://s1-imfile.feishucdn.com/static-resource/v1/";
/// 表情包编号的长度上限（取样约 45 字节）。
const STICKER_KEY_MAX_BYTES: usize = 128;

/// 表情包编号像不像飞书图片 CDN 的编号：`v3_` 开头，只有字母、数字、`_`、`-`。拼进地址前核对，
/// 翻译消息时也用它挡掉认不出的内容（仍记成暂不支持）。
pub(crate) fn is_sticker_key(key: &str) -> bool {
    key.len() <= STICKER_KEY_MAX_BYTES
        && key.len() > AVATAR_ID_PREFIX.len()
        && key.starts_with(AVATAR_ID_PREFIX)
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// 图片 CDN 的错误：地址本身不带令牌，401 / 403 / 404 / 410 都是“这张读不到”，不能当成授权失效（那会停下整组下载）；
/// 限流与服务端错误照常是限流、暂时失败。
fn cdn_error(status: u16) -> ImError {
    match status {
        429 => ImError::new(
            ErrorKind::RateLimited,
            "飞书图片服务请求过于频繁，请稍后重试。",
        ),
        500.. => ImError::new(
            ErrorKind::Disconnected,
            "飞书图片服务暂不可用，请稍后重试。",
        ),
        _ => ImError::new(ErrorKind::NotFound, "这张表情包已无法读取。"),
    }
}

impl FeishuClient {
    /// 打开消息资源的一段（契约 hhh-im client v10 §9）。
    ///
    /// 文件可以从任意位置读，尽早给出总大小（`Content-Range` 或 `inner_file_data_meta`）与内容指纹
    /// （只在 `Content-Disposition` 的文件名本身是内容的 SHA-256 时有，多数文件没有）；图片只能整份读。
    /// 会话归属由调用方核对（本地库有这条消息时以本地为准），这里不再读一次消息。
    ///
    /// # Errors
    /// 消息不属于当前连接、编号格式不对、图片要求分段、平台读不到或拒绝时返回错误；
    /// 平台拒绝续传位置、或没有从要的位置开始给时返回 [`ErrorKind::InvalidRequest`]。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example(client: &hhh_im_feishu::FeishuClient, resource: hhh_im_feishu::ResourceRef) -> hhh_im_feishu::ImResult<()> {
    /// let mut opened = client.open_resource(&resource, hhh_im_feishu::ResourceRange::default()).await?;
    /// println!("共 {:?} 字节，指纹 {:?}", opened.total, opened.content_sha256);
    /// while let Some(chunk) = opened.download.next_chunk().await? {
    ///     println!("{} 字节", chunk.len());
    /// }
    /// # Ok(()) }
    /// ```
    pub async fn open_resource(
        &self,
        reference: &ResourceRef,
        range: ResourceRange,
    ) -> ImResult<OpenedResource> {
        let resolved = self.forwarded_resource(reference).await?;
        let reference = &resolved;
        if reference.kind == ResourceKind::Sticker {
            // 表情包在图片 CDN 上、不带令牌：不看授权是否过期，只核对归属与编号。
            self.check_conversation(&reference.message.conversation)?;
            validate_id(&reference.message.id)?;
            return self.open_sticker(&reference.key, range).await;
        }
        self.grant.ensure_current()?;
        self.check_conversation(&reference.message.conversation)?;
        validate_id(&reference.message.id)?;
        validate_id(&reference.key)?;
        if reference.kind == ResourceKind::Image && !range.is_whole() {
            return Err(ImError::new(
                ErrorKind::Unsupported,
                "飞书图片不支持分段读取。",
            ));
        }
        let mut request = self
            .download
            .get(self.url(&[
                "open-apis",
                "im",
                "v1",
                "messages",
                &reference.message.id,
                "resources",
                &reference.key,
            ])?)
            .query(&[(
                "type",
                match reference.kind {
                    ResourceKind::Image | ResourceKind::Sticker => "image",
                    ResourceKind::File => "file",
                },
            )])
            .bearer_auth(self.grant.access_token());
        if !range.is_whole() {
            request = request.header(header::RANGE, range_header(range));
        }
        let response = request
            .send()
            .await
            .map_err(|error| ImError::network(error, "无法读取消息附件。"))?;
        if !response.status().is_success() || json_reply(&response) {
            return Err(resource_error(response).await);
        }
        let headers = response.headers();
        let content_range = headers
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_range);
        if !range.is_whole() && content_range.is_none_or(|c| c.start != range.start) {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "飞书没有从续传位置开始给数据。",
            ));
        }
        let total = content_range
            .and_then(|c| c.total)
            .or_else(|| meta_file_size(headers))
            .or_else(|| {
                range
                    .is_whole()
                    .then(|| response.content_length())
                    .flatten()
            });
        let content_sha256 = match reference.kind {
            ResourceKind::File => disposition_sha256(headers),
            ResourceKind::Image | ResourceKind::Sticker => None,
        };
        Ok(OpenedResource {
            // 字节上限由下载层按资源类别管；这里不再另设。
            download: ResourceDownload::new(response, usize::MAX),
            total,
            content_sha256,
        })
    }

    /// 打开一张表情包（契约 client v15 §1）：图片 CDN 上的原图，不带令牌、重定向不跟随、整份读。
    /// 字节是不是图片由下载层在改名前核对（不是图片按读不到）。
    async fn open_sticker(&self, key: &str, range: ResourceRange) -> ImResult<OpenedResource> {
        if !is_sticker_key(key) {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "表情包编号格式不正确。",
            ));
        }
        if !range.is_whole() {
            return Err(ImError::new(
                ErrorKind::Unsupported,
                "表情包不支持分段读取。",
            ));
        }
        let response = self
            .download
            .get(self.sticker_url(key)?)
            .send()
            .await
            .map_err(|error| ImError::network(error, "表情包读取失败，请稍后重试。"))?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            tracing::warn!(status, "飞书图片 CDN 没有给出表情包");
            return Err(cdn_error(status));
        }
        let total = response.content_length();
        Ok(OpenedResource {
            download: ResourceDownload::new(response, usize::MAX),
            total,
            content_sha256: None,
        })
    }

    /// 表情包的地址：真实环境是飞书图片 CDN；配置指向本机 HTTP 测试服务时改用那个服务的同一路径。
    fn sticker_url(&self, key: &str) -> ImResult<Url> {
        let base = Url::parse(&self.config.api_base)
            .ok()
            .filter(|base| base.scheme() == "http")
            .and_then(|base| base.join("/static-resource/v1/").ok())
            .map_or_else(|| STICKER_CDN.to_owned(), String::from);
        Url::parse(&format!("{base}{key}~"))
            .map_err(|_| ImError::new(ErrorKind::InvalidRequest, "表情包编号格式不正确。"))
    }

    /// 打开一个头像（飞书 CDN 上的图片，不带令牌）。只接受飞书 CDN 的 HTTPS 地址，重定向不跟随。
    ///
    /// # Errors
    /// 地址不在飞书 CDN、读不到或平台拒绝时返回错误。
    pub async fn open_avatar(&self, url: &str) -> ImResult<OpenedResource> {
        let url = self.avatar_url(url)?;
        let response = self
            .download
            .get(url)
            .send()
            .await
            .map_err(|error| ImError::network(error, "头像读取失败，请稍后重试。"))?;
        if !response.status().is_success() {
            return Err(super::errors::from_code(
                -1,
                response.status().as_u16(),
                None,
            ));
        }
        let total = response.content_length();
        Ok(OpenedResource {
            download: ResourceDownload::new(response, usize::MAX),
            total,
            content_sha256: None,
        })
    }

    /// 头像地址里的身份（契约 hhh-im client v10 §3）：路径里的 `v3_…` 编号，尺寸（`image_size`）是变体。
    ///
    /// 退路：地址里认不出编号时，用去掉尺寸参数的地址当编号（这时换头像能不能认出来取决于地址会不会变）。
    /// 不是飞书 CDN 的地址返回空。
    ///
    /// # Examples
    ///
    /// ```
    /// let (key, variant) = hhh_im_feishu::FeishuClient::avatar_identity(
    ///     "https://s1-imfile.feishucdn.com/static-resource/v1/v3_00105_9e13~?image_size=72x72&format=png",
    /// )
    /// .unwrap();
    /// assert_eq!((key.as_str(), variant.as_str()), ("v3_00105_9e13~", "72x72"));
    /// ```
    pub fn avatar_identity(url: &str) -> Option<(String, String)> {
        let url = Url::parse(url).ok()?;
        let host = url.host_str()?;
        if url.scheme() != "https"
            || !AVATAR_HOST_SUFFIXES
                .iter()
                .any(|suffix| host.ends_with(suffix))
        {
            return None;
        }
        let variant = url
            .query_pairs()
            .find(|(name, _)| name == AVATAR_SIZE_PARAM)
            .map(|(_, value)| value.into_owned())
            .unwrap_or_default();
        let id = url
            .path_segments()?
            .find(|segment| segment.starts_with(AVATAR_ID_PREFIX))
            .map(str::to_owned);
        let key = id.unwrap_or_else(|| {
            let mut bare = url.clone();
            let rest: Vec<(String, String)> = url
                .query_pairs()
                .filter(|(name, _)| name != AVATAR_SIZE_PARAM)
                .map(|(name, value)| (name.into_owned(), value.into_owned()))
                .collect();
            bare.query_pairs_mut().clear().extend_pairs(rest);
            bare.to_string()
        });
        Some((key, variant))
    }

    /// 头像地址只接受飞书 CDN 的 HTTPS（本机测试服务时也接受同一服务的 HTTP）。
    fn avatar_url(&self, raw: &str) -> ImResult<Url> {
        let url = Url::parse(raw)
            .map_err(|_| ImError::new(ErrorKind::InvalidRequest, "头像地址格式不正确。"))?;
        validate_url(&url)?;
        let host = url.host_str().unwrap_or_default();
        let test_host = Url::parse(&self.config.api_base)
            .ok()
            .and_then(|base| base.host_str().map(str::to_owned));
        let local = url.scheme() == "http" && test_host.as_deref() == Some(host);
        if !local
            && !AVATAR_HOST_SUFFIXES
                .iter()
                .any(|suffix| host.ends_with(suffix))
        {
            return Err(ImError::new(
                ErrorKind::InvalidRequest,
                "头像地址不在飞书 CDN 上。",
            ));
        }
        Ok(url)
    }
}

/// `Range` 请求头：`bytes=起-止`（读到末尾时止为空）。
fn range_header(range: ResourceRange) -> String {
    let end = range
        .len
        .map(|len| range.start.saturating_add(len.max(1)) - 1)
        .map(|end| end.to_string())
        .unwrap_or_default();
    format!("bytes={}-{end}", range.start)
}

/// 资源接口回的是 JSON 错误而不是文件：没有附件头、类型是 JSON（附件本身是 JSON 文件时有附件头）。
fn json_reply(response: &Response) -> bool {
    let headers = response.headers();
    let attachment = headers
        .get(header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("attachment"));
    !attachment
        && headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("application/json"))
}

/// `Content-Range: bytes 起-止/总数`（总数可能是 `*`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContentRange {
    start: u64,
    total: Option<u64>,
}

fn parse_content_range(value: &str) -> Option<ContentRange> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let (span, total) = rest.split_once('/')?;
    let (start, _end) = span.split_once('-')?;
    Some(ContentRange {
        start: start.trim().parse().ok()?,
        total: total.trim().parse().ok(),
    })
}

/// `inner_file_data_meta` 里的 `FileSize`。
fn meta_file_size(headers: &header::HeaderMap) -> Option<u64> {
    let meta: Value =
        serde_json::from_str(headers.get("inner_file_data_meta")?.to_str().ok()?).ok()?;
    meta.get("FileSize")?.as_u64()
}

/// `Content-Disposition` 里的文件名去掉扩展名后是 64 位小写十六进制时，当作内容的 SHA-256（有的客户端按内容
/// 命名上传的文件）；原文件名（多数文件，含非 ASCII 的名字）不是指纹，返回空、照常下载。
fn disposition_sha256(headers: &header::HeaderMap) -> Option<String> {
    let value = headers.get(header::CONTENT_DISPOSITION)?.to_str().ok()?;
    let name = value
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("filename="))?
        .trim_matches('"');
    let stem = name.split('.').next()?;
    (stem.len() == 64 && stem.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')))
        .then(|| stem.to_owned())
}

/// 资源接口的错误：按平台错误码换成下载用的说法，其余照通用规则。
async fn resource_error(response: Response) -> ImError {
    let error = match read_json(response).await {
        Err(error) => error,
        Ok(_) => ImError::new(ErrorKind::ProtocolError, "资源接口返回了 JSON 而不是文件。"),
    };
    match super::errors::code_of(&error).and_then(resource_reason) {
        Some((kind, message)) => {
            let mut graded = ImError::new(kind, message);
            graded.platform_code.clone_from(&error.platform_code);
            graded.retry_after_secs = error.retry_after_secs;
            graded
        }
        None => error,
    }
}

/// 资源接口的平台错误码（234003、99992354、234046 已取样；其余按平台文档）。
fn resource_reason(code: i64) -> Option<(ErrorKind, &'static str)> {
    Some(match code {
        234003 => (
            ErrorKind::NotFound,
            "这条消息里没有这个文件（可能已撤回或删除）。",
        ),
        230110 => (ErrorKind::NotFound, "消息已被删除，读不到里面的文件。"),
        99992354 => (ErrorKind::NotFound, "消息不存在或已删除。"),
        234037 => (ErrorKind::LimitExceeded, "文件超过飞书允许下载的大小。"),
        234038 => (
            ErrorKind::PermissionDenied,
            "这个会话开启了保密模式，不能下载里面的文件。",
        ),
        234040 => (
            ErrorKind::PermissionDenied,
            "这条消息对你不可见，读不到里面的文件。",
        ),
        234043 => (ErrorKind::Unsupported, "这类消息里的资源不支持下载。"),
        234046 => (ErrorKind::InvalidRequest, "续传位置超出了文件末尾。"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> header::HeaderMap {
        let mut map = header::HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().unwrap());
        }
        map
    }

    /// 取样得到的文件响应头（原样）。
    const SAMPLED_META: &str = r#"{"Mime":"audio/mpeg","FileName":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.mp3","CreateTime":1790577120,"FileSize":4363344,"IsShardingFile":false}"#;
    const SAMPLED_DISPOSITION: &str = r#"attachment; filename="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.mp3""#;

    #[test]
    fn sticker_keys_must_look_like_image_cdn_keys_and_cdn_refusals_are_not_auth_failures() {
        // 取样的四个编号（回溯 v59 §2-1）都认；别的前缀、路径符号、空、过长的不认。
        for key in [
            "v3_00t4_11111111-1111-4111-8111-11111111111g",
            "v3_0010a_22222222-2222-4222-8222-22222222222g",
            "v3_00rp_33333333-3333-4333-8333-33333333333g",
            "v3_00nv_44444444-4444-4444-8444-44444444444g",
        ] {
            assert!(is_sticker_key(key), "{key}");
        }
        for key in [
            "",
            "v3_",
            "img_v3_0215v_x",
            "v3_a/b",
            "v3_a~",
            "v3_a?x=1",
            &format!("v3_{}", "a".repeat(200)),
        ] {
            assert!(!is_sticker_key(key), "{key}");
        }
        // CDN 不带令牌：401 / 403 / 404 都是“这张读不到”（永久），不能当成授权失效停下整组下载。
        for status in [401, 403, 404, 410, 302] {
            assert_eq!(cdn_error(status).kind, ErrorKind::NotFound, "{status}");
        }
        assert_eq!(cdn_error(429).kind, ErrorKind::RateLimited);
        assert_eq!(cdn_error(503).kind, ErrorKind::Disconnected);
    }

    #[test]
    fn sampled_file_headers_give_the_size_and_the_content_fingerprint() {
        let sampled = headers(&[
            ("content-disposition", SAMPLED_DISPOSITION),
            ("inner_file_data_meta", SAMPLED_META),
        ]);
        assert_eq!(meta_file_size(&sampled), Some(4_363_344));
        assert_eq!(
            disposition_sha256(&sampled).as_deref(),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
        // 文件名不是指纹（别的平台、改过名的）时不当指纹。
        let named = headers(&[("content-disposition", "attachment; filename=report.json")]);
        assert_eq!(disposition_sha256(&named), None);
        // 经开放平台接口上传的文件：飞书给的是原文件名，中文按 UTF-8 原样放在头里（im-files v1 验收实测）。
        let mut uploaded = header::HeaderMap::new();
        uploaded.insert(
            header::CONTENT_DISPOSITION,
            header::HeaderValue::from_bytes(
                "attachment; filename=\"示例测试-文件卡验收.txt\"".as_bytes(),
            )
            .unwrap(),
        );
        assert_eq!(disposition_sha256(&uploaded), None);
        assert_eq!(meta_file_size(&headers(&[])), None);
    }

    #[test]
    fn content_ranges_and_range_requests_follow_the_sampled_shapes() {
        assert_eq!(
            parse_content_range("bytes 1024-4363343/4363344"),
            Some(ContentRange {
                start: 1024,
                total: Some(4_363_344)
            })
        );
        assert_eq!(
            parse_content_range("bytes 0-99/*"),
            Some(ContentRange {
                start: 0,
                total: None
            })
        );
        assert_eq!(parse_content_range("items 0-1/2"), None);
        let segment = |start, len| range_header(ResourceRange { start, len });
        assert_eq!(segment(0, Some(1024)), "bytes=0-1023");
        assert_eq!(segment(1024, None), "bytes=1024-");
        assert_eq!(segment(0, Some(RESOURCE_SEGMENT_BYTES)), "bytes=0-33554431");
    }

    #[test]
    fn resource_error_codes_become_download_reasons() {
        assert_eq!(resource_reason(234003).unwrap().0, ErrorKind::NotFound);
        assert_eq!(resource_reason(99992354).unwrap().0, ErrorKind::NotFound);
        assert_eq!(resource_reason(230110).unwrap().0, ErrorKind::NotFound);
        assert_eq!(resource_reason(234037).unwrap().0, ErrorKind::LimitExceeded);
        assert_eq!(
            resource_reason(234038).unwrap().0,
            ErrorKind::PermissionDenied
        );
        assert_eq!(
            resource_reason(234040).unwrap().0,
            ErrorKind::PermissionDenied
        );
        assert_eq!(resource_reason(234043).unwrap().0, ErrorKind::Unsupported);
        assert_eq!(
            resource_reason(234046).unwrap().0,
            ErrorKind::InvalidRequest
        );
        assert_eq!(resource_reason(99991663), None, "其余照通用规则");
    }

    #[test]
    fn avatar_identity_is_the_v3_id_and_the_size_is_the_variant() {
        let sampled = "https://s1-imfile.feishucdn.com/static-resource/v1/v3_00105_1a2b3c4d-0000-4000-8000-00000000a01g~?image_size=72x72&cut_type=&quality=&format=png&sticker_format=.webp";
        let bigger = sampled.replace("72x72", "240x240");
        let (key, variant) = FeishuClient::avatar_identity(sampled).unwrap();
        assert_eq!(key, "v3_00105_1a2b3c4d-0000-4000-8000-00000000a01g~");
        assert_eq!(variant, "72x72");
        let (same, larger) = FeishuClient::avatar_identity(&bigger).unwrap();
        assert_eq!(same, key, "同一个头像换尺寸，编号不变");
        assert_eq!(larger, "240x240");
        // 退路：认不出编号时，用去掉尺寸参数的地址。
        let (fallback, size) = FeishuClient::avatar_identity(
            "https://sf3-cn.feishucdn.com/obj/avatar/abc.png?image_size=72x72&format=png",
        )
        .unwrap();
        assert_eq!(
            fallback,
            "https://sf3-cn.feishucdn.com/obj/avatar/abc.png?format=png"
        );
        assert_eq!(size, "72x72");
        assert_eq!(
            FeishuClient::avatar_identity("https://evil.example.com/v3_x?image_size=72x72"),
            None
        );
        assert_eq!(
            FeishuClient::avatar_identity("http://s1-imfile.feishucdn.com/v3_x"),
            None
        );
    }
}
