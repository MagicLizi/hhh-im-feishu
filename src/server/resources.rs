//! 资源（protocol-v1 §9）：按段读消息资源、头像、表情图，上传要发送的图片与文件。
//!
//! - 读：宿主每次要一段（≤1 MiB），按顺序一段接一段来。插件为一个资源开一条 HTTP 读流（文件从要的位置读到末尾），
//!   下一段正好接着上一段时接着读这条流，不再重开请求；位置对不上（宿主重试、换了位置）就重开。读流按资源记着，
//!   至多 [`STREAMS_MAX`] 条、闲置 [`STREAM_IDLE`] 就丢掉。飞书声明文件按 32 MiB 一段续传（同内置），宿主每一大段里按
//!   1 MiB 来要，都落在同一条读流上。
//! - 表情图：目录里有的按目录里的地址取；目录外的按飞书的命名规律猜（同 HHH 内置时宿主的猜图）。
//! - 上传：字节先在内存里攒齐（图片 ≤10 MB、文件 ≤30 MB，与发送上限相同），核对长度与 SHA-256 后整份交给飞书，
//!   拿到的资源编号就是 `uploads/finish` 的 `key`。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use hhh_im_protocol::method as pm;
use hhh_im_protocol::rpc::RpcError;
use serde_json::{Value, json};
use sha2::Digest as _;

use super::{CONNECTION, Server, errors, wire};
use crate::*;

/// 同时记着几条读流。
const STREAMS_MAX: usize = 8;
/// 读流闲置多久丢掉（连接闲久了可能已被中间设备断掉）。
const STREAM_IDLE: Duration = Duration::from_secs(60);
/// 一段最多多少字节（协议 §14）。
const SEGMENT_MAX: u64 = 1024 * 1024;
/// 同时进行的上传最多几个（每个都在内存里攒字节）。
const UPLOADS_MAX: usize = 4;

/// 一条正在读的资源流。
struct Stream {
    /// 是哪个资源（会话、消息、资源键、种类；头像是地址）。
    id: String,
    /// 下一个字节在整个资源里的位置。
    position: u64,
    download: ResourceDownload,
    /// 读出来还没交出去的字节。
    buffer: Vec<u8>,
    total: Option<u64>,
    sha256: Option<String>,
    ended: bool,
    used: Instant,
}

/// 一个在攒字节的上传。
struct Upload {
    kind: ResourceKind,
    name: String,
    bytes: u64,
    data: Vec<u8>,
}

/// 读流与上传。
#[derive(Default)]
pub(crate) struct Resources {
    streams: std::sync::Mutex<Vec<Stream>>,
    uploads: std::sync::Mutex<HashMap<String, Upload>>,
    next_upload: std::sync::atomic::AtomicU64,
    /// 表情目录（补全过的）：表情图按类型找地址用。
    catalog: tokio::sync::Mutex<Option<EmojiCatalog>>,
}

impl Resources {
    /// 取出接着这个位置的读流（取出来读，读完再放回）。
    fn take(&self, id: &str, offset: u64) -> Option<Stream> {
        let mut streams = self.streams.lock().expect("读流表锁中毒");
        let index = streams
            .iter()
            .position(|stream| stream.id == id && stream.position == offset)?;
        Some(streams.swap_remove(index))
    }

    /// 放回还没读完的读流；丢掉闲置太久的，超过上限时丢最旧的。
    fn keep(&self, stream: Stream) {
        let mut streams = self.streams.lock().expect("读流表锁中毒");
        streams.retain(|old| old.used.elapsed() < STREAM_IDLE && old.id != stream.id);
        if streams.len() >= STREAMS_MAX
            && let Some(oldest) = streams
                .iter()
                .enumerate()
                .min_by_key(|(_, stream)| stream.used)
                .map(|(index, _)| index)
        {
            streams.swap_remove(oldest);
        }
        streams.push(stream);
    }

    /// 记下补全过的表情目录。
    pub(crate) async fn remember_catalog(&self, catalog: EmojiCatalog) {
        *self.catalog.lock().await = Some(catalog);
    }
}

/// 从读流里交出一段：攒够 `length` 字节或读到末尾；能判断是不是末尾就判断。
async fn read_segment(stream: &mut Stream, length: u64) -> ImResult<pm::ResourceChunk> {
    let length = usize::try_from(length.min(SEGMENT_MAX)).unwrap_or(usize::MAX);
    while stream.buffer.len() < length && !stream.ended {
        match stream.download.next_chunk().await? {
            Some(chunk) => stream.buffer.extend_from_slice(&chunk),
            None => stream.ended = true,
        }
    }
    let take = length.min(stream.buffer.len());
    let data: Vec<u8> = stream.buffer.drain(..take).collect();
    stream.position += data.len() as u64;
    stream.used = Instant::now();
    // 不知道总长、这一段刚好读满时，再看一眼后面还有没有（有就留着下一段用）。
    if stream.buffer.is_empty() && !stream.ended && stream.total.is_none() {
        match stream.download.next_chunk().await? {
            Some(chunk) => stream.buffer.extend_from_slice(&chunk),
            None => stream.ended = true,
        }
    }
    let eof = (stream.ended && stream.buffer.is_empty())
        || stream.total.is_some_and(|total| stream.position >= total);
    Ok(pm::ResourceChunk {
        data: BASE64.encode(&data),
        total: stream.total,
        sha256: stream.sha256.clone(),
        eof,
    })
}

/// 刚打开的读流跳到要的位置（整份读的图片、头像被要求从中间读时）。
async fn skip(stream: &mut Stream, offset: u64) -> ImResult<()> {
    let mut left = usize::try_from(offset).unwrap_or(usize::MAX);
    while left > 0 && !stream.ended {
        if stream.buffer.is_empty() {
            match stream.download.next_chunk().await? {
                Some(chunk) => stream.buffer.extend_from_slice(&chunk),
                None => stream.ended = true,
            }
        }
        let drop = left.min(stream.buffer.len());
        stream.buffer.drain(..drop);
        left -= drop;
    }
    stream.position = offset;
    Ok(())
}

fn stream_of(id: String, opened: OpenedResource, position: u64) -> Stream {
    Stream {
        id,
        position,
        download: opened.download,
        buffer: Vec::new(),
        total: opened.total,
        sha256: opened.content_sha256,
        ended: false,
        used: Instant::now(),
    }
}

fn to_value(chunk: pm::ResourceChunk) -> Value {
    serde_json::to_value(chunk).expect("资源分段总能写成 JSON")
}

impl Server {
    /// `resources/read`。
    pub(crate) async fn read_resource(
        &self,
        params: pm::ResourcesReadParams,
    ) -> Result<Value, RpcError> {
        let reference = ResourceRef {
            message: MessageRef {
                conversation: ConversationRef::new(CONNECTION, &params.conversation_id),
                id: params.message_id.clone(),
            },
            key: params.key.clone(),
            kind: wire::resource_kind_in(params.kind),
        };
        let id = format!(
            "resource\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{:?}",
            params.conversation_id, params.message_id, params.key, reference.kind
        );
        if let Some(stream) = self.resources.take(&id, params.offset) {
            return self.finish_read(stream, params.length).await;
        }
        // 文件能从中间读（Range），从要的位置读到末尾；图片与表情包只能整份读，从头读、跳到要的位置。
        let ranged = reference.kind == ResourceKind::File && params.offset > 0;
        let range = ResourceRange {
            start: if ranged { params.offset } else { 0 },
            len: None,
        };
        let opened = self
            .with_ready_client(
                |client| async move { client.open_resource(&reference, range).await },
            )
            .await?;
        let mut stream = stream_of(id, opened, range.start);
        if !ranged && params.offset > 0 {
            skip(&mut stream, params.offset)
                .await
                .map_err(|error| errors::rpc(&error))?;
        }
        self.finish_read(stream, params.length).await
    }

    /// `avatars/read`：头像键就是飞书的头像地址。
    pub(crate) async fn read_avatar(
        &self,
        params: pm::AvatarsReadParams,
    ) -> Result<Value, RpcError> {
        let id = format!("avatar\u{1f}{}", params.key);
        if let Some(stream) = self.resources.take(&id, params.offset) {
            return self.finish_read(stream, params.length).await;
        }
        let url = params.key.clone();
        let opened = self
            .with_ready_client(|client| async move { client.open_avatar(&url).await })
            .await?;
        let mut stream = stream_of(id, opened, 0);
        if params.offset > 0 {
            skip(&mut stream, params.offset)
                .await
                .map_err(|error| errors::rpc(&error))?;
        }
        self.finish_read(stream, params.length).await
    }

    /// 交出一段；没读完的读流放回去，等下一段接着读。
    async fn finish_read(&self, mut stream: Stream, length: u64) -> Result<Value, RpcError> {
        let chunk = read_segment(&mut stream, length)
            .await
            .map_err(|error| errors::rpc(&error))?;
        if !chunk.eof {
            self.resources.keep(stream);
        }
        Ok(to_value(chunk))
    }

    /// `emoji/image`：目录里的按目录的地址取，目录外的按命名规律依次猜；都读不到回“没有”（宿主写名字）。
    pub(crate) async fn read_emoji_image(
        &self,
        params: pm::EmojiImageParams,
    ) -> Result<Value, RpcError> {
        let known = self.emoji_entry(&params.emoji_type).await;
        let candidates = match known {
            Some(emoji) => vec![emoji],
            None => crate::facts::guess_emoji_images(&params.emoji_type),
        };
        if candidates.is_empty() {
            return Err(errors::of(ErrorKind::NotFound, "表情目录里没有这个表情。"));
        }
        let guessing = candidates.len() > 1;
        let mut bytes = None;
        for candidate in candidates {
            let result = self
                .with_ready_client(|client| async move { client.emoji_image(&candidate).await })
                .await;
            match result {
                Ok(found) => {
                    bytes = Some(found);
                    break;
                }
                Err(error)
                    if guessing
                        && error.data.as_ref().is_some_and(|data| {
                            data.kind == hhh_im_protocol::ErrorKind::NotFound
                        }) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
        let Some(bytes) = bytes else {
            tracing::info!(emoji = %params.emoji_type, "目录里没有的表情猜不到图，界面写类型名");
            return Err(errors::of(
                ErrorKind::NotFound,
                "表情目录里没有这个表情，也猜不到它的图。",
            ));
        };
        let start = usize::try_from(params.offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let length = usize::try_from(params.length.min(SEGMENT_MAX)).unwrap_or(usize::MAX);
        let end = start.saturating_add(length).min(bytes.len());
        Ok(to_value(pm::ResourceChunk {
            data: BASE64.encode(&bytes[start..end]),
            total: Some(bytes.len() as u64),
            sha256: None,
            eof: end >= bytes.len(),
        }))
    }

    /// 表情目录里这个类型的一项；这个进程还没读过目录时读一次（读不到就按猜图）。
    async fn emoji_entry(&self, emoji_type: &str) -> Option<EmojiInfo> {
        let mut catalog = self.resources.catalog.lock().await;
        if catalog.is_none() {
            match self
                .with_ready_client(|client| async move { client.emoji_catalog().await })
                .await
            {
                Ok(fresh) => *catalog = Some(crate::facts::complete_emoji_catalog(fresh)),
                Err(error) => {
                    tracing::warn!(error = %error.message, "表情目录读不到，表情图按命名规律猜");
                    return None;
                }
            }
        }
        catalog
            .as_ref()?
            .emojis
            .iter()
            .find(|emoji| emoji.emoji_type == emoji_type)
            .cloned()
    }

    /// `uploads/begin`：核对种类与大小（同发送上限，说法同适配器的上传），开一个上传。
    pub(crate) fn upload_begin(&self, params: pm::UploadsBeginParams) -> Result<Value, RpcError> {
        let kind = wire::resource_kind_in(params.kind);
        let max = match kind {
            ResourceKind::Image => IMAGE_UPLOAD_MAX_BYTES,
            ResourceKind::File => FILE_UPLOAD_MAX_BYTES,
            ResourceKind::Sticker => {
                return Err(errors::of(ErrorKind::Unsupported, "表情包不能上传。"));
            }
        };
        if params.bytes == 0 || params.bytes > max as u64 {
            return Err(errors::of(
                ErrorKind::LimitExceeded,
                "图片须在 10 MB 以内、文件须在 30 MB 以内，且不能为空。",
            ));
        }
        let mut uploads = self.resources.uploads.lock().expect("上传表锁中毒");
        if uploads.len() >= UPLOADS_MAX {
            return Err(errors::of(
                ErrorKind::RateLimited,
                "同时上传的附件太多，请稍后再试。",
            ));
        }
        let id = format!(
            "upload-{}",
            self.resources
                .next_upload
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1
        );
        uploads.insert(
            id.clone(),
            Upload {
                kind,
                name: params.name,
                bytes: params.bytes,
                data: Vec::new(),
            },
        );
        Ok(json!({ "uploadId": id }))
    }

    /// `uploads/chunk`：按顺序接上一段。
    pub(crate) fn upload_chunk(&self, params: pm::UploadsChunkParams) -> Result<Value, RpcError> {
        let bytes = BASE64.decode(params.data.as_bytes()).map_err(|_| {
            errors::of(ErrorKind::InvalidRequest, "上传的这一段不是合法的 base64。")
        })?;
        let mut uploads = self.resources.uploads.lock().expect("上传表锁中毒");
        let upload = uploads
            .get_mut(&params.upload_id)
            .ok_or_else(|| errors::of(ErrorKind::NotFound, "没有这个上传，请重新上传。"))?;
        if params.offset != upload.data.len() as u64 {
            return Err(errors::of(
                ErrorKind::InvalidRequest,
                "上传的这一段没有接着上一段。",
            ));
        }
        if bytes.len() as u64 > SEGMENT_MAX
            || upload.data.len() as u64 + bytes.len() as u64 > upload.bytes
        {
            return Err(errors::of(
                ErrorKind::LimitExceeded,
                "上传的内容比说好的长。",
            ));
        }
        upload.data.extend_from_slice(&bytes);
        Ok(json!({}))
    }

    /// `uploads/finish`：核对长度与 SHA-256，整份交给飞书，回资源编号。
    pub(crate) async fn upload_finish(
        &self,
        params: pm::UploadsFinishParams,
    ) -> Result<Value, RpcError> {
        let upload = self
            .resources
            .uploads
            .lock()
            .expect("上传表锁中毒")
            .remove(&params.upload_id)
            .ok_or_else(|| errors::of(ErrorKind::NotFound, "没有这个上传，请重新上传。"))?;
        if upload.data.len() as u64 != upload.bytes {
            return Err(errors::of(
                ErrorKind::InvalidRequest,
                "上传的内容比说好的短。",
            ));
        }
        let digest: String = sha2::Sha256::digest(&upload.data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if !digest.eq_ignore_ascii_case(params.sha256.trim()) {
            return Err(errors::of(
                ErrorKind::InvalidRequest,
                "上传的内容与摘要对不上。",
            ));
        }
        let (kind, name, data) = (upload.kind, upload.name, upload.data);
        let key = self
            .with_client(|client| {
                let (name, data) = (name.clone(), data.clone());
                async move { client.upload_resource(kind, &name, data).await }
            })
            .await?;
        Ok(json!({ "key": key }))
    }

    /// `uploads/abort`。
    pub(crate) fn upload_abort(&self, params: pm::UploadsAbortParams) -> Result<Value, RpcError> {
        self.resources
            .uploads
            .lock()
            .expect("上传表锁中毒")
            .remove(&params.upload_id);
        Ok(json!({}))
    }
}
