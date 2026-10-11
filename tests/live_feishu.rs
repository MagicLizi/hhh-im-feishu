use hhh_im_feishu::*;

/// 只读实测，必须由操作者显式提供本人令牌并使用 --ignored 运行。
#[tokio::test]
#[ignore = "需要真实飞书应用和本人用户授权；不发送消息"]
async fn live_identity_groups_and_optional_history() {
    let token = std::env::var("FEISHU_USER_ACCESS_TOKEN").expect("请显式提供本人测试令牌");
    let client = FeishuClient::connect(
        FeishuConfig::default(),
        "live-test",
        AuthGrant::from_access_token(token).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(client.account().user.kind, "user");
    client.list_conversations(10, None).await.unwrap();
    if let Ok(id) = std::env::var("FEISHU_TEST_CHAT_ID") {
        client
            .list_messages(&MessageQuery::recent(ConversationRef::new("live-test", id)))
            .await
            .unwrap();
    }
}
