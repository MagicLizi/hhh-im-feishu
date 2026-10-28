//! 读取与动作（protocol-v1 §8、§10）：每个方法换成飞书适配器的一次调用，结果翻成线上类型。
//!
//! 读取的结果都带 `platformRequests`（契约 provider-protocol v14）：这次调用向飞书发了几个请求（适配器在查询出口
//! 记的数，同 HHH 内置时调度器记账的口径），宿主据此记进读取额度。

use std::sync::Arc;

use hhh_im_protocol::method as pm;
use hhh_im_protocol::rpc::RpcError;
use serde_json::{Value, json};

use super::{CONNECTION, Server, errors, wire};
use crate::*;

/// 读取结果里“这次向平台发了几个请求”的字段。
const PLATFORM_REQUESTS: &str = "platformRequests";
/// 记着多少个发送结果（同一个操作编号只发一次）。
const SENDS_REMEMBERED: usize = 1024;

/// 发过的消息：操作编号 → 结果（同一个编号再来时回同一个结果，§10）。
#[derive(Default)]
pub(crate) struct Sends {
    done: tokio::sync::Mutex<Vec<(String, Arc<tokio::sync::OnceCell<Value>>)>>,
}

/// 跑一次读取，数它向飞书发了几个请求，写进结果。
async fn counted(
    read: impl std::future::Future<Output = Result<Value, RpcError>>,
) -> Result<Value, RpcError> {
    let (result, sent) = crate::metering::counted(read).await;
    result.map(|mut value| {
        if let Some(object) = value.as_object_mut() {
            object.insert(PLATFORM_REQUESTS.into(), sent.into());
        }
        value
    })
}

fn to_value(value: impl serde::Serialize) -> Value {
    serde_json::to_value(value).expect("结果总能写成 JSON")
}

fn conversation_ref(id: &str) -> ConversationRef {
    ConversationRef::new(CONNECTION, id)
}

fn message_ref(conversation: &str, id: &str) -> MessageRef {
    MessageRef {
        conversation: conversation_ref(conversation),
        id: id.to_owned(),
    }
}

impl Server {
    /// `conversations/list`（只列用户所在的群，飞书的群列表不含单聊）。
    pub(crate) async fn conversations_list(
        &self,
        params: pm::ConversationsListParams,
    ) -> Result<Value, RpcError> {
        let cursor = wire::cursor_in(params.cursor.as_deref())?;
        counted(async {
            let page = self
                .with_client(|client| {
                    let cursor = cursor.clone();
                    async move {
                        client
                            .list_conversations(params.limit, cursor.as_ref())
                            .await
                    }
                })
                .await?;
            Ok(to_value(pm::Page {
                items: page.items.iter().map(wire::conversation).collect(),
                next: wire::cursor_out(page.next.as_ref()),
            }))
        })
        .await
    }

    /// `conversations/get`。
    pub(crate) async fn conversations_get(
        &self,
        params: pm::ConversationsGetParams,
    ) -> Result<Value, RpcError> {
        let reference = conversation_ref(&params.conversation_id);
        counted(async {
            let conversation = self
                .with_client(|client| {
                    let reference = reference.clone();
                    async move { client.get_conversation(&reference).await }
                })
                .await?;
            Ok(to_value(wire::conversation(&conversation)))
        })
        .await
    }

    /// `conversations/search`。
    pub(crate) async fn conversations_search(
        &self,
        params: pm::SearchParams,
    ) -> Result<Value, RpcError> {
        let query = SearchQuery {
            text: params.text,
            limit: params.limit,
            cursor: wire::cursor_in(params.cursor.as_deref())?,
        };
        counted(async {
            let page = self
                .with_client(|client| {
                    let query = query.clone();
                    async move { client.search_conversations(&query).await }
                })
                .await?;
            Ok(to_value(pm::SearchPage {
                items: page.items.iter().map(wire::conversation).collect(),
                next: wire::cursor_out(page.next.as_ref()),
                notice: page.notice,
            }))
        })
        .await
    }

    /// `conversations/members`。
    pub(crate) async fn conversations_members(
        &self,
        params: pm::MembersParams,
    ) -> Result<Value, RpcError> {
        let reference = conversation_ref(&params.conversation_id);
        let cursor = wire::cursor_in(params.cursor.as_deref())?;
        counted(async {
            let page = self
                .with_client(|client| {
                    let (reference, cursor) = (reference.clone(), cursor.clone());
                    async move {
                        client
                            .list_members(&reference, params.limit, cursor.as_ref())
                            .await
                    }
                })
                .await?;
            Ok(to_value(pm::Page {
                items: page.items.iter().map(wire::person).collect(),
                next: wire::cursor_out(page.next.as_ref()),
            }))
        })
        .await
    }

    /// `conversations/findDirect`：只读定位对方发来过消息的既有单聊。
    pub(crate) async fn conversations_find_direct(
        &self,
        params: pm::FindDirectParams,
    ) -> Result<Value, RpcError> {
        let user = UserRef {
            connection_id: CONNECTION.into(),
            id: params.user_id,
        };
        counted(async {
            let found = self
                .with_client(|client| {
                    let user = user.clone();
                    async move { client.find_direct_conversation(&user).await }
                })
                .await?;
            Ok(to_value(pm::FindDirectResult {
                conversation_id: found.conversation.map(|c| c.id),
                notice: found.notice,
            }))
        })
        .await
    }

    /// `conversations/recentActivity`（会话发现）。
    pub(crate) async fn conversations_recent_activity(
        &self,
        params: pm::RecentActivityParams,
    ) -> Result<Value, RpcError> {
        let query = ActivityQuery {
            since_ms: params.since_ms,
            pages_max: params.pages_max,
        };
        counted(async {
            let activity = self
                .with_client(|client| async move { client.recent_activity(&query).await })
                .await?;
            Ok(to_value(pm::RecentActivityResult {
                items: activity
                    .items
                    .iter()
                    .take(hhh_im_protocol::budget::RECENT_ACTIVITY_ITEMS_MAX)
                    .map(wire::activity)
                    .collect(),
                complete: activity.complete,
            }))
        })
        .await
    }

    /// `people/get`：按编号批量读人；没读到的放进 `missing`（同 HHH 内置：平台没返回的人算没有）。
    pub(crate) async fn people_get(&self, params: pm::PeopleGetParams) -> Result<Value, RpcError> {
        let users: Vec<UserRef> = params
            .ids
            .iter()
            .map(|id| UserRef {
                connection_id: CONNECTION.into(),
                id: id.clone(),
            })
            .collect();
        counted(async {
            let found = if users.is_empty() {
                Vec::new()
            } else {
                self.with_client(|client| {
                    let users = users.clone();
                    async move { client.users(&users).await }
                })
                .await?
            };
            let mut missing: Vec<String> = Vec::new();
            for id in &params.ids {
                if !found.iter().any(|user| user.reference.id == *id) && !missing.contains(id) {
                    missing.push(id.clone());
                }
            }
            Ok(to_value(pm::PeopleGetResult {
                found: found.iter().map(wire::person).collect(),
                missing,
            }))
        })
        .await
    }

    /// `people/search`。
    pub(crate) async fn people_search(&self, params: pm::SearchParams) -> Result<Value, RpcError> {
        let query = SearchQuery {
            text: params.text,
            limit: params.limit,
            cursor: wire::cursor_in(params.cursor.as_deref())?,
        };
        counted(async {
            let page = self
                .with_client(|client| {
                    let query = query.clone();
                    async move { client.search_users(&query).await }
                })
                .await?;
            Ok(to_value(pm::SearchPage {
                items: page.items.iter().map(wire::person).collect(),
                next: wire::cursor_out(page.next.as_ref()),
                notice: page.notice,
            }))
        })
        .await
    }

    /// `messages/list`：毫秒换回适配器的秒（宿主给的是整秒乘 1000；不是整秒时起点往前、终点往后取整，不漏）。
    pub(crate) async fn messages_list(
        &self,
        params: pm::MessagesListParams,
    ) -> Result<Value, RpcError> {
        let query = MessageQuery {
            conversation: conversation_ref(&params.conversation_id),
            thread_id: params.thread_id,
            start_time: params.after_ms.map(|ms| ms.div_euclid(1000)),
            end_time: params.before_ms.map(|ms| (ms + 999).div_euclid(1000)),
            newest_first: params.newest_first,
            limit: params.limit,
            cursor: wire::cursor_in(params.cursor.as_deref())?,
        };
        counted(async {
            let page = self
                .with_client(|client| {
                    let query = query.clone();
                    async move { client.list_messages(&query).await }
                })
                .await?;
            let mut items = Vec::with_capacity(page.items.len());
            for message in &page.items {
                items.push(self.forwarded_message(message).await);
            }
            Ok(to_value(pm::Page {
                items,
                next: wire::cursor_out(page.next.as_ref()),
            }))
        })
        .await
    }

    /// `messages/get`。
    pub(crate) async fn messages_get(
        &self,
        params: pm::MessagesGetParams,
    ) -> Result<Value, RpcError> {
        let reference = message_ref(&params.conversation_id, &params.message_id);
        counted(async {
            let message = self
                .with_client(|client| {
                    let reference = reference.clone();
                    async move { client.get_message(&reference).await }
                })
                .await?;
            Ok(to_value(self.forwarded_message(&message).await))
        })
        .await
    }

    /// `reactions/list`。缺权限时给用户的话写清要开哪个权限、怎么补（同 HHH 内置飞书的 `reactions_tip`）。
    pub(crate) async fn reactions_list(
        &self,
        params: pm::ReactionsListParams,
    ) -> Result<Value, RpcError> {
        let query = ReactionQuery {
            message: message_ref(&params.conversation_id, &params.message_id),
            limit: params.limit,
            cursor: wire::cursor_in(params.cursor.as_deref())?,
        };
        counted(async {
            let page = self
                .with_client(|client| {
                    let query = query.clone();
                    async move { client.list_reactions(&query).await }
                })
                .await
                .map_err(|mut error| {
                    if let Some(data) = error.data.as_mut()
                        && data.kind == hhh_im_protocol::ErrorKind::PermissionDenied
                    {
                        data.user_message = Some(crate::facts::reauth::REACTIONS_TIP.into());
                    }
                    error
                })?;
            Ok(to_value(pm::Page {
                items: page.items.iter().map(wire::reaction).collect(),
                next: wire::cursor_out(page.next.as_ref()),
            }))
        })
        .await
    }

    /// `emoji/catalog`：开放平台目录补上随包快照里的表情与中文名（同 HHH 内置时宿主读目录时做的那一步）。
    pub(crate) async fn emoji_catalog(&self) -> Result<Value, RpcError> {
        counted(async {
            let catalog = self
                .with_client(|client| async move { client.emoji_catalog().await })
                .await?;
            let catalog = crate::facts::complete_emoji_catalog(catalog);
            let result = wire::emoji_catalog_json(&catalog);
            self.resources.remember_catalog(catalog).await;
            Ok(result)
        })
        .await
    }

    /// `messages/send`：同一个操作编号只发一次（操作编号就是飞书的去重 uuid），再来时回同一个结果。
    /// 平台拒绝（没发出去）的不记，宿主可以拿同一个编号再试。
    pub(crate) async fn messages_send(
        &self,
        params: pm::MessagesSendParams,
    ) -> Result<Value, RpcError> {
        let cell = {
            let mut done = self.sends.done.lock().await;
            match done.iter().find(|(id, _)| *id == params.operation_id) {
                Some((_, cell)) => cell.clone(),
                None => {
                    if done.len() >= SENDS_REMEMBERED {
                        done.remove(0);
                    }
                    let cell = Arc::new(tokio::sync::OnceCell::new());
                    done.push((params.operation_id.clone(), cell.clone()));
                    cell
                }
            }
        };
        let request = SendRequest {
            operation_id: params.operation_id.clone(),
            target: wire::target_in(params.target, CONNECTION),
            content: params
                .content
                .into_iter()
                .map(wire::outgoing_in)
                .collect::<Result<_, _>>()
                .map_err(errors::invalid_params)?,
        };
        let result = cell
            .get_or_try_init(|| async {
                let outcome = self
                    .with_client(|client| {
                        let request = request.clone();
                        async move { client.send(&request).await }
                    })
                    .await?;
                Ok::<_, RpcError>(to_value(wire::send_outcome(&outcome)))
            })
            .await;
        match result {
            Ok(value) => Ok(value.clone()),
            Err(error) => {
                self.sends
                    .done
                    .lock()
                    .await
                    .retain(|(id, kept)| !(*id == params.operation_id && Arc::ptr_eq(kept, &cell)));
                Err(error)
            }
        }
    }

    /// `messages/recall`。
    pub(crate) async fn messages_recall(
        &self,
        params: pm::MessagesRecallParams,
    ) -> Result<Value, RpcError> {
        let reference = message_ref(&params.conversation_id, &params.message_id);
        self.with_client(|client| {
            let reference = reference.clone();
            async move { client.recall(&reference).await }
        })
        .await?;
        Ok(json!({}))
    }

    /// `reactions/add`。
    pub(crate) async fn reactions_add(
        &self,
        params: pm::ReactionsAddParams,
    ) -> Result<Value, RpcError> {
        let reference = message_ref(&params.conversation_id, &params.message_id);
        let reaction = self
            .with_client(|client| {
                let (reference, emoji) = (reference.clone(), params.emoji_type.clone());
                async move { client.add_reaction(&reference, &emoji).await }
            })
            .await?;
        Ok(to_value(wire::reaction(&reaction)))
    }

    /// `reactions/remove`。
    pub(crate) async fn reactions_remove(
        &self,
        params: pm::ReactionsRemoveParams,
    ) -> Result<Value, RpcError> {
        let reference = message_ref(&params.conversation_id, &params.message_id);
        self.with_client(|client| {
            let (reference, id) = (reference.clone(), params.reaction_id.clone());
            async move { client.remove_reaction(&reference, &id).await }
        })
        .await?;
        Ok(json!({}))
    }

    /// `messages/lookupSend`：飞书没有按发送 uuid 查询的接口（同内置，能力里也没声明）。
    pub(crate) async fn messages_lookup_send(&self) -> Result<Value, RpcError> {
        let outcome = self
            .with_client(|client| async move { client.lookup_send("").await })
            .await?;
        Ok(to_value(wire::send_outcome(&outcome)))
    }
}

/// 读宿主给的参数；读不懂时是参数错误。
pub(crate) fn params<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, RpcError> {
    serde_json::from_value(value).map_err(errors::invalid_params)
}
