//! 飞书 IM 插件的库：统一消息类型、身份授权和飞书接口封装（从 HHH 的 hhh-im 原样搬来）。
//!
//! 飞书适配器从 HHH 内置的飞书搬来，行为一样：
//! - `im`（都从本库根目录导出）：hhh-im 里飞书要用的那部分——统一消息类型、错误、授权、统一接口；
//! - `net`：hhh-net 的统一 TLS 握手（飞书会单独掐断小握手，必须照搬）；
//! - `metering`：数一次宿主调用里向飞书发了几次请求；
//! - `feishu`：飞书适配器本体，私有响应格式在里面翻成统一类型；
//! - `server`：协议服务（HHH-IM-Protocol v1）——请求循环、登录、续期、交接，统一类型翻成线上类型。
//!
//! 库只消费调用方注入的配置和凭据，不保存凭据（凭据交给宿主存）。
//!
//! # Examples
//!
//! ```
//! use hhh_im_feishu::{ConversationRef, MessageQuery};
//! let chat = ConversationRef::new("personal", "oc_example");
//! let query = MessageQuery::recent(chat);
//! assert_eq!(query.limit, 20);
//! ```

//! 网络调用由调用方的异步运行时驱动，凭据只在运行时注入：
//!
//! ```no_run
//! # async fn example() -> hhh_im_feishu::ImResult<()> {
//! use hhh_im_feishu::{AuthGrant, FeishuClient, FeishuConfig, ConversationRef, MessageQuery};
//! let grant = AuthGrant::from_access_token("user_access_token")?;
//! let client = FeishuClient::connect(FeishuConfig::default(), "personal", grant).await?;
//! let query = MessageQuery::recent(ConversationRef::new("personal", "oc_example"));
//! let page = client.list_messages(&query).await?;
//! assert!(page.items.len() <= usize::from(query.limit));
//! # Ok(()) }
//! ```
//!
//! 名称搜索保持分页与账号范围；私聊定位只读，不会给对方发消息：
//!
//! ```no_run
//! # async fn search(client: &hhh_im_feishu::FeishuClient) -> hhh_im_feishu::ImResult<()> {
//! let page = client.search_users(&hhh_im_feishu::SearchQuery::new("张")).await?;
//! if let Some(user) = page.items.first() {
//!     let found = client.find_direct_conversation(&user.reference).await?;
//!     // found.conversation 为 None 仅表示没有命中可搜索的既有私聊。
//!     println!("{:?}", found.conversation);
//! }
//! # Ok(()) }
//! ```

mod feishu;
mod im;
// 平台请求记账：数一次宿主调用里向飞书发了几次请求（随结果报给宿主）。
pub mod metering;
// 统一 TLS 握手的客户端起点（照搬 hhh-net）。
pub mod net;
// 协议服务：HHH-IM-Protocol 的请求循环、登录、续期、交接，以及统一类型翻成线上类型。
pub mod server;
// 测试用：从 hhh-im 本地消息库搬来的几个判断（飞书的翻译测试拿它们核对结果）。
#[cfg(test)]
mod store;

pub use feishu::{
    EMOJI_IMAGE_MAX_BYTES, FILE_UPLOAD_MAX_BYTES, FeishuAuth, FeishuClient, FeishuConfig,
    FeishuEvents, IMAGE_UPLOAD_MAX_BYTES, MEMBER_PAGE_MAX, OPERATION_ID_MAX_BYTES,
    RESOURCE_SEGMENT_BYTES, ResourceDownload, WebhookDelivery, facts,
};
pub use im::*;
#[cfg(test)]
use store::thread_root_of;
