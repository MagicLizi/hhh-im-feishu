//! 飞书对统一接口的实现（契约 hhh-im client v17 §3、§6）。
//!
//! 平台说明（原来这里的 `PLATFORM`）搬到了 [`super::facts`]，成了普通常量与函数。

use crate::*;

impl ImClient for FeishuClient {
    async fn list_reactions(&self, query: &ReactionQuery) -> ImResult<Page<MessageReaction>> {
        FeishuClient::list_reactions(self, query).await
    }
    async fn add_reaction(
        &self,
        message: &MessageRef,
        emoji_type: &str,
    ) -> ImResult<MessageReaction> {
        FeishuClient::add_reaction(self, message, emoji_type).await
    }
    async fn remove_reaction(&self, message: &MessageRef, reaction_id: &str) -> ImResult<()> {
        FeishuClient::remove_reaction(self, message, reaction_id).await
    }
    async fn emoji_catalog(&self) -> ImResult<EmojiCatalog> {
        FeishuClient::emoji_catalog(self).await
    }
    async fn emoji_image(&self, emoji: &EmojiInfo) -> ImResult<Vec<u8>> {
        FeishuClient::emoji_image(self, emoji).await
    }
    async fn search_conversations(
        &self,
        query: &SearchQuery,
    ) -> ImResult<SearchPage<Conversation>> {
        FeishuClient::search_conversations(self, query).await
    }
    async fn search_users(&self, query: &SearchQuery) -> ImResult<SearchPage<SearchUser>> {
        FeishuClient::search_users(self, query).await
    }
    async fn find_direct_conversation(&self, user: &UserRef) -> ImResult<DirectConversationLookup> {
        FeishuClient::find_direct_conversation(self, user).await
    }
    async fn recent_activity(&self, query: &ActivityQuery) -> ImResult<RecentActivity> {
        FeishuClient::recent_activity(self, query).await
    }
    fn account(&self) -> &Account {
        FeishuClient::account(self)
    }
    fn capabilities(&self) -> CapabilitySet {
        FeishuClient::capabilities(self)
    }
    fn content_repairs(&self) -> Vec<ContentRepair> {
        crate::feishu::CONTENT_REPAIRS.to_vec()
    }
    fn content_rereads(&self) -> Vec<ContentReread> {
        vec![
            crate::feishu::CARD_REREAD,
            crate::feishu::RICH_POST_REREAD,
            crate::feishu::STICKER_REREAD,
            crate::feishu::CARD_V2_REREAD,
            crate::feishu::CARD_TABLE_REREAD,
            crate::feishu::MENTION_ALL_RICH_REREAD,
            crate::feishu::CARD_MENTION_REREAD,
            crate::feishu::VIDEO_REREAD,
        ]
    }
    async fn list_conversations(
        &self,
        limit: u16,
        cursor: Option<&PageCursor>,
    ) -> ImResult<Page<Conversation>> {
        FeishuClient::list_conversations(self, limit, cursor).await
    }
    async fn get_conversation(&self, conversation: &ConversationRef) -> ImResult<Conversation> {
        FeishuClient::get_conversation(self, conversation).await
    }
    async fn list_messages(&self, query: &MessageQuery) -> ImResult<Page<ImMessage>> {
        FeishuClient::list_messages(self, query).await
    }
    async fn get_message(&self, message: &MessageRef) -> ImResult<ImMessage> {
        FeishuClient::get_message(self, message).await
    }
    async fn send(&self, request: &SendRequest) -> ImResult<SendOutcome> {
        FeishuClient::send(self, request).await
    }
    async fn lookup_send(&self, operation_id: &str) -> ImResult<SendOutcome> {
        FeishuClient::lookup_send(self, operation_id).await
    }
    async fn open_resource(
        &self,
        resource: &ResourceRef,
        range: ResourceRange,
    ) -> ImResult<OpenedResource> {
        FeishuClient::open_resource(self, resource, range).await
    }
    async fn open_avatar(&self, url: &str) -> ImResult<OpenedResource> {
        FeishuClient::open_avatar(self, url).await
    }
    async fn list_members(
        &self,
        conversation: &ConversationRef,
        limit: u16,
        cursor: Option<&PageCursor>,
    ) -> ImResult<Page<SearchUser>> {
        FeishuClient::list_members(self, conversation, limit, cursor).await
    }
    async fn users(&self, users: &[UserRef]) -> ImResult<Vec<SearchUser>> {
        FeishuClient::users(self, users).await
    }
    async fn upload_resource(
        &self,
        kind: ResourceKind,
        name: &str,
        bytes: Vec<u8>,
    ) -> ImResult<String> {
        FeishuClient::upload_resource(self, kind, name, bytes).await
    }
    async fn recall(&self, message: &MessageRef) -> ImResult<()> {
        FeishuClient::recall(self, message).await
    }
}
