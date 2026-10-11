//! 从 HHH 的 hhh-im 搬来的公共部分：飞书适配器与它的测试用到的统一消息类型、错误、授权与统一接口。
//!
//! 名字与 serde 形状一字不改：翻译结果要与测试夹具（`*.expected.json`）逐字节相同，第二步再由协议层
//! 把它们翻成协议的线上类型。HHH 宿主才有的部分（本地消息库、同步、平台登记、连接表单、参数声明）没有搬。

pub(crate) mod auth;
pub(crate) mod client;
mod error;
mod types;

pub use auth::{AuthGrant, LoginRequest, PendingLogin, random_id};
pub use client::{ImClient, OpenedResource, ResourceRange, RetranslateItem};
pub use error::{ErrorKind, ImError, ImResult, ReauthReason};
pub use types::*;
