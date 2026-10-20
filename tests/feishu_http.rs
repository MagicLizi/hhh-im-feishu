use hhh_im_feishu::*;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path, query_param},
};

fn config(server: &MockServer) -> FeishuConfig {
    FeishuConfig {
        api_base: server.uri(),
        authorize_url: format!("{}/authorize", server.uri()),
        token_url: format!("{}/oauth/v3/token", server.uri()),
    }
}
fn raw_message() -> Value {
    serde_json::from_str(include_str!("fixtures/feishu/response_text.json")).unwrap()
}
fn chat() -> ConversationRef {
    ConversationRef::new("test", "oc_group")
}
fn message_ref() -> MessageRef {
    MessageRef {
        conversation: chat(),
        id: "om_reply".into(),
    }
}
async fn setup() -> (MockServer, FeishuClient) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/open-apis/authen/v1/user_info"))
        .and(header("authorization", "Bearer user_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"code":0,"data":{"open_id":"ou_me","name":"Alice","tenant_key":"tenant"}}),
        ))
        .mount(&server)
        .await;
    let client = FeishuClient::connect(
        config(&server),
        "test",
        AuthGrant::from_access_token("user_token").unwrap(),
    )
    .await
    .unwrap();
    (server, client)
}
fn request(operation: &str) -> SendRequest {
    SendRequest {
        operation_id: operation.into(),
        target: SendTarget::Conversation {
            conversation: chat(),
        },
        content: vec![OutgoingContent::Text {
            text: "Hello & <world>".into(),
        }],
    }
}

fn search_fixtures() -> (Value, Value) {
    (
        serde_json::from_str(include_str!("fixtures/search_responses.json")).unwrap(),
        serde_json::from_str(include_str!("fixtures/search_responses.expected.json")).unwrap(),
    )
}

fn target_user() -> UserRef {
    UserRef {
        connection_id: "test".into(),
        id: "ou_target".into(),
    }
}

async fn mock_reaction_message(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages/om_reply"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":0,"data":{"items":[raw_message()]}})),
        )
        .mount(server)
        .await;
}

fn reaction_fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/feishu/reactions/list.json")).unwrap()
}

#[tokio::test]
async fn reactions_use_user_token_and_match_independent_fixture() {
    let (server, client) = setup().await;
    mock_reaction_message(&server).await;
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages/om_reply/reactions"))
        .and(header("authorization", "Bearer user_token"))
        .and(query_param("user_id_type", "open_id"))
        .and(query_param("page_size", "50"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":reaction_fixture()})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let page = client
        .list_reactions(&ReactionQuery::new(message_ref()))
        .await
        .unwrap();
    let expected: Value =
        serde_json::from_str(include_str!("fixtures/feishu/reactions/list.expected.json")).unwrap();
    assert_eq!(serde_json::to_value(&page.items).unwrap(), expected);
    let decoded: Vec<MessageReaction> = serde_json::from_value(expected).unwrap();
    assert_eq!(decoded, page.items);
    assert!(page.next.is_none());
}

#[tokio::test]
async fn reaction_cursors_bind_message_connection_and_limit_before_network() {
    let (server, client) = setup().await;
    mock_reaction_message(&server).await;
    Mock::given(path("/open-apis/im/v1/messages/om_reply/reactions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"code":0,"data":{"items":[],"has_more":true,"page_token":"next"}}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let mut query = ReactionQuery::new(message_ref());
    query.cursor = client.list_reactions(&query).await.unwrap().next;
    let request_count = server.received_requests().await.unwrap().len();
    for case in 0..5 {
        let mut invalid = query.clone();
        match case {
            0 => invalid.message.conversation.connection_id = "other".into(),
            1 => invalid.message.conversation.id = "oc_other".into(),
            2 => invalid.message.id = "om_other".into(),
            3 => invalid.limit = 1,
            _ => invalid.limit = 0,
        }
        assert_eq!(
            client.list_reactions(&invalid).await.unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
    }
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        request_count
    );
    server.reset().await;
    mock_reaction_message(&server).await;
    Mock::given(path("/open-apis/im/v1/messages/om_reply/reactions"))
        .and(query_param("page_token", "next"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":0,"data":{"items":[],"has_more":false}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert!(client.list_reactions(&query).await.unwrap().next.is_none());
}

#[tokio::test]
async fn reactions_reject_wrong_conversation_and_malformed_partial_pages() {
    let (server, client) = setup().await;
    let mut wrong = raw_message();
    wrong["chat_id"] = json!("oc_other");
    let guard = Mock::given(path("/open-apis/im/v1/messages/om_reply"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":{"items":[wrong]}})),
        )
        .expect(1)
        .mount_as_scoped(&server)
        .await;
    assert_eq!(
        client
            .list_reactions(&ReactionQuery::new(message_ref()))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::ProtocolError
    );
    assert!(
        !server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|r| r.url.path().ends_with("/reactions"))
    );
    drop(guard);
    mock_reaction_message(&server).await;
    for case in 0..8 {
        let mut data = reaction_fixture();
        match case {
            0 => {
                data.as_object_mut().unwrap().remove("has_more");
            }
            1 => data["has_more"] = json!(true),
            2 => data["items"][0]["operator"]["operator_type"] = json!("unknown"),
            3 => data["items"][0]["action_time"] = json!("bad"),
            4 => data["items"][0]["reaction_type"] = json!({}),
            5 => data["items"][1] = data["items"][0].clone(),
            6 => data["items"] = json!(vec![data["items"][0].clone(); 51]),
            _ => {
                data.as_object_mut().unwrap().remove("items");
            }
        }
        let guard = Mock::given(path("/open-apis/im/v1/messages/om_reply/reactions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":data})))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        assert_eq!(
            client
                .list_reactions(&ReactionQuery::new(message_ref()))
                .await
                .unwrap_err()
                .kind,
            ErrorKind::ProtocolError,
            "case {case}"
        );
        drop(guard);
    }
}

#[tokio::test]
async fn reactions_explain_scope_and_preserve_rate_limit_and_missing_message() {
    let (server, client) = setup().await;
    mock_reaction_message(&server).await;
    for (code, kind) in [
        (99991672, ErrorKind::PermissionDenied),
        (231008, ErrorKind::PermissionDenied),
        (231003, ErrorKind::NotFound),
        (99991400, ErrorKind::RateLimited),
    ] {
        let guard = Mock::given(path("/open-apis/im/v1/messages/om_reply/reactions"))
            .respond_with(
                ResponseTemplate::new(400)
                    .insert_header("retry-after", "7")
                    .set_body_json(json!({"code":code})),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let error = client
            .list_reactions(&ReactionQuery::new(message_ref()))
            .await
            .unwrap_err();
        assert_eq!(error.kind, kind);
        assert_eq!(error.platform_code, Some(code.to_string()));
        assert_eq!(error.retry_after_secs, Some(7));
        if kind == ErrorKind::PermissionDenied {
            assert!(error.message.contains("login --reactions"));
        }
        drop(guard);
    }
    let known = FeishuClient::connect(
        config(&server),
        "test",
        AuthGrant::from_parts(
            "user_token".into(),
            None,
            None,
            Some(vec!["im:message".into()]),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let count = server.received_requests().await.unwrap().len();
    assert_eq!(
        known
            .list_reactions(&ReactionQuery::new(message_ref()))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::PermissionDenied
    );
    assert_eq!(server.received_requests().await.unwrap().len(), count);
}

#[tokio::test]
async fn search_expired_cursor_and_page_cap_explain_recovery() {
    let (server, client) = setup().await;
    for (code, kind, hint) in [
        (231020, ErrorKind::InvalidRequest, "第一页"),
        (231022, ErrorKind::LimitExceeded, "更具体"),
    ] {
        let mock = Mock::given(path("/open-apis/im/v2/chats/search"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({"code":code})))
            .expect(1)
            .mount_as_scoped(&server)
            .await;
        let error = client
            .search_conversations(&SearchQuery::new("Ada"))
            .await
            .unwrap_err();
        assert_eq!(error.kind, kind);
        assert!(error.message.contains(hint));
        drop(mock);
    }
}

#[tokio::test]
async fn search_rejects_oversized_and_malformed_pages_but_allows_empty() {
    let (server, client) = setup().await;
    let mut query = SearchQuery::new("Ada");
    query.limit = 1;
    for (route, key) in [
        ("/open-apis/im/v2/chats/search", "items"),
        ("/open-apis/search/v1/user", "users"),
    ] {
        for data in [
            json!({}),
            json!({key:[{},{}]}),
            json!({key:[{}]}),
            json!({key:[],"has_more":true}),
        ] {
            let mock = Mock::given(path(route))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":data})),
                )
                .mount_as_scoped(&server)
                .await;
            let error = if key == "items" {
                client.search_conversations(&query).await.unwrap_err()
            } else {
                client.search_users(&query).await.unwrap_err()
            };
            assert_eq!(error.kind, ErrorKind::ProtocolError);
            drop(mock);
        }
        let mock = Mock::given(path(route))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"code":0,"data":{key:[],"has_more":false}})),
            )
            .mount_as_scoped(&server)
            .await;
        if key == "items" {
            assert!(
                client
                    .search_conversations(&query)
                    .await
                    .unwrap()
                    .items
                    .is_empty()
            );
        } else {
            assert!(client.search_users(&query).await.unwrap().items.is_empty());
        }
        drop(mock);
    }
}

#[tokio::test]
async fn search_uses_official_user_requests_and_fixture_formats() {
    let (server, client) = setup().await;
    let (raw, expected) = search_fixtures();
    Mock::given(method("POST")).and(path("/open-apis/im/v2/chats/search"))
        .and(header("authorization", "Bearer user_token"))
        .and(query_param("user_id_type", "open_id")).and(query_param("page_size", "20"))
        .and(body_json(json!({"query":"研发 团队","filter":{"disable_search_by_user":true,"search_types":["private","external","public_joined"]}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":raw["groups"]}))).expect(1).mount(&server).await;
    let groups = client
        .search_conversations(&SearchQuery::new("研发 团队"))
        .await
        .unwrap();
    assert_eq!(serde_json::to_value(groups).unwrap(), expected["groups"]);
    Mock::given(method("GET"))
        .and(path("/open-apis/search/v1/user"))
        .and(header("authorization", "Bearer user_token"))
        .and(query_param("query", "测试 用户"))
        .and(query_param("page_size", "20"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":raw["users"]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let users = client
        .search_users(&SearchQuery::new("测试 用户"))
        .await
        .unwrap();
    assert_eq!(serde_json::to_value(users).unwrap(), expected["users"]);
    Mock::given(method("POST"))
        .and(path("/open-apis/im/v1/messages/search"))
        .and(header("authorization", "Bearer user_token"))
        .and(query_param("user_id_type", "open_id"))
        .and(query_param("page_size", "1"))
        .and(body_json(
            json!({"filter":{"from_ids":["ou_target"],"from_types":["user"],"chat_type":"p2p"}}),
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":raw["direct"]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let direct = client
        .find_direct_conversation(&target_user())
        .await
        .unwrap();
    assert_eq!(serde_json::to_value(direct).unwrap(), expected["direct"]);
    // 只有身份和三个只读搜索请求，没有发送、建会话或读取整段私聊历史。
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
}

#[tokio::test]
async fn search_cursors_bind_kind_keyword_limit_and_connection() {
    let (server, client) = setup().await;
    for (route, key) in [
        ("/open-apis/im/v2/chats/search", "items"),
        ("/open-apis/search/v1/user", "users"),
    ] {
        Mock::given(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"code":0,"data":{key:[],"has_more":true,"page_token":"next"}}),
            ))
            .expect(2)
            .mount(&server)
            .await;
    }
    let mut query = SearchQuery::new("Ada");
    query.cursor = client.search_conversations(&query).await.unwrap().next;
    client.search_conversations(&query).await.unwrap();
    assert_eq!(
        client.search_users(&query).await.unwrap_err().kind,
        ErrorKind::InvalidRequest
    );
    let original = query.clone();
    query.text = "Other".into();
    assert!(client.search_conversations(&query).await.is_err());
    query = original.clone();
    query.limit = 10;
    assert!(client.search_conversations(&query).await.is_err());
    let other = FeishuClient::connect(
        config(&server),
        "other",
        AuthGrant::from_access_token("user_token").unwrap(),
    )
    .await
    .unwrap();
    assert!(other.search_conversations(&original).await.is_err());
    query = SearchQuery::new("Ada");
    query.cursor = client.search_users(&query).await.unwrap().next;
    client.search_users(&query).await.unwrap();
    assert!(client.search_conversations(&query).await.is_err());
    assert!(other.search_users(&query).await.is_err());
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r
                .url
                .query_pairs()
                .any(|(k, v)| k == "page_token" && v == "next"))
            .count(),
        2
    );
}

#[tokio::test]
async fn search_rejects_invalid_inputs_and_self_before_network() {
    let (server, client) = setup().await;
    for text in ["".to_owned(), "   ".into(), "x\ny".into(), "中".repeat(51)] {
        let query = SearchQuery::new(text);
        assert_eq!(
            client.search_conversations(&query).await.unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
        assert_eq!(
            client.search_users(&query).await.unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
    }
    for limit in [0, 21] {
        let mut query = SearchQuery::new("Ada");
        query.limit = limit;
        assert!(client.search_users(&query).await.is_err());
    }
    for user in [
        UserRef {
            connection_id: "other".into(),
            ..target_user()
        },
        UserRef {
            id: "ou_me".into(),
            ..target_user()
        },
        UserRef {
            id: "../bad".into(),
            ..target_user()
        },
    ] {
        assert_eq!(
            client
                .find_direct_conversation(&user)
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidRequest
        );
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn search_empty_results_and_untrusted_direct_matches_stay_explicit() {
    let (server, client) = setup().await;
    let route = "/open-apis/im/v1/messages/search";
    let mock = Mock::given(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"code":0,"data":{"items":[],"has_more":false,"notice":"索引可能不完整"}}),
        ))
        .mount_as_scoped(&server)
        .await;
    let found = client
        .find_direct_conversation(&target_user())
        .await
        .unwrap();
    assert!(found.conversation.is_none());
    assert_eq!(found.notice.as_deref(), Some("索引可能不完整"));
    drop(mock);
    for data in [
        json!({"items":[{"meta_data":{"from_id":"ou_other","chat_id":"oc_direct","is_p2p_chat":true}}]}),
        json!({"items":[{"meta_data":{"from_id":"ou_target","chat_id":"oc_group","is_p2p_chat":false}}]}),
        json!({"items":[{"meta_data":{"from_id":"ou_target","is_p2p_chat":true}}]}),
        json!({"items":[{},{}]}),
        json!({"items":[{}]}),
    ] {
        let mock = Mock::given(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":data})))
            .mount_as_scoped(&server)
            .await;
        assert_eq!(
            client
                .find_direct_conversation(&target_user())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::ProtocolError
        );
        drop(mock);
    }
}

#[tokio::test]
async fn search_permissions_explain_reauthorization_without_retry() {
    let (server, client) = setup().await;
    for route in [
        "/open-apis/search/v1/user",
        "/open-apis/im/v1/messages/search",
    ] {
        Mock::given(path(route))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({"code":99991672})))
            .expect(1)
            .mount(&server)
            .await;
    }
    let error = client
        .search_users(&SearchQuery::new("Ada"))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::PermissionDenied);
    assert_eq!(error.platform_code.as_deref(), Some("99991672"));
    assert!(
        error.message.contains("contact:user:search") && error.message.contains("login --search")
    );
    let error = client
        .find_direct_conversation(&target_user())
        .await
        .unwrap_err();
    assert!(error.message.contains("search:message"));
    let known = FeishuClient::connect(
        config(&server),
        "test",
        AuthGrant::from_parts(
            "user_token".into(),
            None,
            None,
            Some(vec!["im:chat:readonly".into()]),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        known
            .search_users(&SearchQuery::new("Ada"))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::PermissionDenied
    );
    assert_eq!(
        known
            .find_direct_conversation(&target_user())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::PermissionDenied
    );
    // 两次身份请求，加两次明确失败的搜索；已知缺权限不再打网络。
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
}

#[tokio::test]
async fn pagination_preserves_order_and_binds_query_and_connection() {
    let (server, client) = setup().await;
    let mut older = raw_message();
    older["message_id"] = "om_older".into();
    older["create_time"] = "1700000000000".into();
    Mock::given(method("GET")).and(path("/open-apis/im/v1/messages"))
        .and(query_param("container_id_type", "chat")).and(query_param("container_id", "oc_group"))
        .and(query_param("sort_type", "ByCreateTimeDesc")).and(query_param("page_size", "20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":{"items":[raw_message(), older],"has_more":true,"page_token":"next"}}))).expect(2).mount(&server).await;
    let mut query = MessageQuery::recent(chat());
    let page = client.list_messages(&query).await.unwrap();
    assert_eq!(page.items[0].reference.id, "om_older");
    query.cursor = page.next;
    client.list_messages(&query).await.unwrap();
    query.limit = 10;
    assert_eq!(
        client.list_messages(&query).await.unwrap_err().kind,
        ErrorKind::InvalidRequest
    );
    query.limit = 20;
    query.conversation.id = "oc_other".into();
    assert!(client.list_messages(&query).await.is_err());
    query.conversation = ConversationRef::new("other", "oc_group");
    assert!(client.list_messages(&query).await.is_err());
    let requests = server.received_requests().await.unwrap();
    assert!(requests.iter().any(|r| {
        r.url
            .query_pairs()
            .any(|(k, v)| k == "page_token" && v == "next")
    }));
}

#[tokio::test]
async fn groups_and_thread_queries_use_official_parameters() {
    let (server, client) = setup().await;
    Mock::given(path("/open-apis/im/v1/chats")).and(query_param("page_size", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":{"items":[{"chat_id":"oc_group","name":"Project","avatar":"https://example.test/a"}],"has_more":false}}))).expect(1).mount(&server).await;
    assert_eq!(
        client.list_conversations(2, None).await.unwrap().items[0].kind,
        ConversationKind::Group
    );
    Mock::given(path("/open-apis/im/v1/chats/oc_group"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":0,"data":{"name":"Project","chat_mode":"group"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client.get_conversation(&chat()).await.unwrap().reference,
        chat()
    );
    Mock::given(path("/open-apis/im/v1/messages"))
        .and(query_param("container_id_type", "thread"))
        .and(query_param("container_id", "omt_topic"))
        .and(query_param("sort_type", "ByCreateTimeDesc"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":0,"data":{"items":[raw_message()],"has_more":false}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    // 按话题读不带时间范围（飞书不支持），只能按页从最新往前读。
    let mut query = MessageQuery::recent(chat());
    query.thread_id = Some("omt_topic".into());
    assert_eq!(
        client.list_messages(&query).await.unwrap().items[0]
            .thread_id
            .as_deref(),
        Some("omt_topic")
    );
}

#[tokio::test]
async fn page_limits_and_wrong_chat_responses_fail() {
    let (server, client) = setup().await;
    let mut raw = raw_message();
    raw["chat_id"] = "oc_wrong".into();
    Mock::given(path("/open-apis/im/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":{"items":[raw]}})),
        )
        .mount(&server)
        .await;
    for limit in [0, 51] {
        let mut query = MessageQuery::recent(chat());
        query.limit = limit;
        assert_eq!(
            client.list_messages(&query).await.unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
    }
    assert!(client.list_conversations(101, None).await.is_err());
    assert_eq!(
        client
            .list_messages(&MessageQuery::recent(chat()))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::ProtocolError
    );
}

#[tokio::test]
async fn send_and_reply_use_user_token_exact_content_and_stable_uuid() {
    let (server, client) = setup().await;
    Mock::given(method("POST")).and(path("/open-apis/im/v1/messages")).and(header("authorization", "Bearer user_token"))
        .and(query_param("receive_id_type", "chat_id"))
        .and(body_json(json!({"receive_id":"oc_group","msg_type":"text","content":"{\"text\":\"Hello & <world>\"}","uuid":"stable"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":raw_message()}))).expect(1).mount(&server).await;
    assert!(matches!(
        client.send(&request("stable")).await.unwrap(),
        SendOutcome::Accepted { .. }
    ));
    Mock::given(path("/open-apis/im/v1/messages/om_reply"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":0,"data":{"items":[raw_message()]}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST")).and(path("/open-apis/im/v1/messages/om_reply/reply"))
        .and(body_json(json!({"msg_type":"text","content":"{\"text\":\"Hello & <world>\"}","uuid":"reply","reply_in_thread":true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":raw_message()}))).expect(1).mount(&server).await;
    let mut reply = request("reply");
    reply.target = SendTarget::Reply {
        message: message_ref(),
        in_thread: true,
        thread_root: None,
        also_to_conversation: false,
    };
    assert!(matches!(
        client.send(&reply).await.unwrap(),
        SendOutcome::Accepted { .. }
    ));
}

#[tokio::test]
async fn ambiguous_sends_are_unknown_and_never_retried() {
    for response in [
        ResponseTemplate::new(503),
        ResponseTemplate::new(408),
        ResponseTemplate::new(200).set_body_string("broken"),
        ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":{}})),
    ] {
        let (server, client) = setup().await;
        Mock::given(method("POST"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        assert!(matches!(
            client.send(&request("stable")).await.unwrap(),
            SendOutcome::Unknown { .. }
        ));
        assert_eq!(
            client.lookup_send("stable").await.unwrap_err().kind,
            ErrorKind::Unsupported
        );
    }
}

#[tokio::test]
async fn errors_are_classified_redacted_and_not_retried() {
    for (status, code, kind) in [
        (403, 230006, ErrorKind::PermissionDenied),
        (401, 99991663, ErrorKind::AuthExpired),
        (429, 99991400, ErrorKind::RateLimited),
        (404, 230001, ErrorKind::NotFound),
    ] {
        let (server, client) = setup().await;
        Mock::given(path("/open-apis/im/v1/chats"))
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header("Retry-After", "7")
                    .set_body_json(json!({"code":code,"msg":"SECRET_TOKEN"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let error = client.list_conversations(20, None).await.unwrap_err();
        assert_eq!(error.kind, kind);
        assert_eq!(error.retry_after_secs, Some(7));
        assert!(!format!("{error:?}").contains("SECRET"));
    }
    let (server, client) = setup().await;
    Mock::given(path("/open-apis/im/v1/chats"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "2")
                .set_body_string("throttled"),
        )
        .mount(&server)
        .await;
    assert_eq!(
        client.list_conversations(20, None).await.unwrap_err().kind,
        ErrorKind::RateLimited
    );
}

#[tokio::test]
async fn redirect_and_oversized_responses_do_not_escape_budget() {
    for response in [
        ResponseTemplate::new(302).insert_header("Location", "/must-not-follow"),
        ResponseTemplate::new(200).set_body_string("x".repeat(4 * 1024 * 1024 + 1)),
    ] {
        let (server, client) = setup().await;
        Mock::given(path("/open-apis/im/v1/chats"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        assert!(client.list_conversations(20, None).await.is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }
}

fn file_ref(key: &str) -> ResourceRef {
    ResourceRef {
        message: message_ref(),
        key: key.into(),
        kind: ResourceKind::File,
    }
}

async fn read_all(opened: OpenedResource) -> Vec<u8> {
    let mut download = opened.download;
    let mut bytes = vec![];
    while let Some(chunk) = download.next_chunk().await.unwrap() {
        bytes.extend(chunk);
    }
    bytes
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 取样得到的文件响应头形状（2026-09-29 真实只读取样，文件名即内容的 SHA-256）。
fn sampled_file(body: &[u8], status: u16) -> ResponseTemplate {
    let sha = sha256_hex(body);
    ResponseTemplate::new(status)
        .insert_header(
            "Content-Disposition",
            format!("attachment; filename=\"{sha}.mp3\"").as_str(),
        )
        .insert_header(
            "inner_file_data_meta",
            format!(
                "{{\"Mime\":\"audio/mpeg\",\"FileName\":\"{sha}.mp3\",\"CreateTime\":1790577120,\"FileSize\":{},\"IsShardingFile\":false}}",
                body.len()
            )
            .as_str(),
        )
        .set_body_raw(body.to_vec(), "audio/mpeg")
}

fn sampled_error(name: &str) -> Value {
    let all: Value =
        serde_json::from_str(include_str!("fixtures/feishu/resources/errors.json")).unwrap();
    all[name].clone()
}

#[tokio::test]
async fn a_file_gives_its_size_and_content_fingerprint_before_the_body() {
    let (server, client) = setup().await;
    let body: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    Mock::given(path("/open-apis/im/v1/messages/om_reply/resources/file_1"))
        .and(query_param("type", "file"))
        .and(header("authorization", "Bearer user_token"))
        .respond_with(sampled_file(&body, 200))
        .expect(1)
        .mount(&server)
        .await;
    let opened = client
        .open_resource(&file_ref("file_1"), ResourceRange::default())
        .await
        .unwrap();
    assert_eq!(opened.total, Some(4096));
    assert_eq!(
        opened.content_sha256.as_deref(),
        Some(sha256_hex(&body).as_str())
    );
    assert_eq!(read_all(opened).await, body);
    // 会话归属由下载层核对：这里不再读一次消息。
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_json_file_is_still_a_file() {
    let (server, client) = setup().await;
    Mock::given(path("/open-apis/im/v1/messages/om_reply/resources/file_1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Disposition", "attachment; filename=report.json")
                .set_body_raw(b"{\"answer\":42}".to_vec(), "application/json"),
        )
        .mount(&server)
        .await;
    let opened = client
        .open_resource(&file_ref("file_1"), ResourceRange::default())
        .await
        .unwrap();
    assert_eq!(opened.content_sha256, None, "文件名不是指纹时不当指纹");
    assert_eq!(read_all(opened).await, b"{\"answer\":42}");
}

#[tokio::test]
async fn a_range_is_asked_for_and_the_reply_must_start_there() {
    let (server, client) = setup().await;
    let body: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    Mock::given(path("/open-apis/im/v1/messages/om_reply/resources/file_1"))
        .and(header("range", "bytes=1024-"))
        .respond_with(
            sampled_file(&body[1024..], 206).insert_header("Content-Range", "bytes 1024-4095/4096"),
        )
        .mount(&server)
        .await;
    // 像图片那样：带 Range 却回 206 整份、没有 Content-Range。
    Mock::given(path(
        "/open-apis/im/v1/messages/om_reply/resources/file_whole",
    ))
    .respond_with(sampled_file(&body, 206))
    .mount(&server)
    .await;
    let tail = client
        .open_resource(
            &file_ref("file_1"),
            ResourceRange {
                start: 1024,
                len: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(tail.total, Some(4096), "总数取自 Content-Range");
    assert_eq!(read_all(tail).await, body[1024..]);
    let mismatched = client
        .open_resource(
            &file_ref("file_whole"),
            ResourceRange {
                start: 1024,
                len: None,
            },
        )
        .await;
    assert!(matches!(
        mismatched,
        Err(ImError {
            kind: ErrorKind::InvalidRequest,
            ..
        })
    ));
}

#[tokio::test]
async fn sampled_resource_errors_are_graded_for_downloads() {
    let (server, client) = setup().await;
    for (key, fixture) in [
        ("file_gone", "fileNotInMessage"),
        ("file_nomsg", "messageNotFound"),
        ("file_range", "rangeNotSatisfiable"),
    ] {
        Mock::given(path(format!(
            "/open-apis/im/v1/messages/om_reply/resources/{key}"
        )))
        .respond_with(ResponseTemplate::new(400).set_body_json(sampled_error(fixture)))
        .mount(&server)
        .await;
    }
    Mock::given(path(
        "/open-apis/im/v1/messages/om_reply/resources/file_busy",
    ))
    .respond_with(ResponseTemplate::new(503))
    .mount(&server)
    .await;
    let kind = |key: &'static str| {
        let client = &client;
        async move {
            client
                .open_resource(&file_ref(key), ResourceRange::default())
                .await
                .err()
                .map(|e| (e.kind, e.platform_code))
        }
    };
    assert_eq!(
        kind("file_gone").await,
        Some((ErrorKind::NotFound, Some("234003".to_owned())))
    );
    assert_eq!(
        kind("file_nomsg").await,
        Some((ErrorKind::NotFound, Some("99992354".to_owned())))
    );
    assert_eq!(
        kind("file_range").await,
        Some((ErrorKind::InvalidRequest, Some("234046".to_owned())))
    );
    assert_eq!(
        kind("file_busy").await.map(|k| k.0),
        Some(ErrorKind::Disconnected)
    );
}

#[tokio::test]
async fn images_are_only_read_whole() {
    let (server, client) = setup().await;
    let image = ResourceRef {
        message: message_ref(),
        key: "img_1".into(),
        kind: ResourceKind::Image,
    };
    let refused = client
        .open_resource(
            &image,
            ResourceRange {
                start: 10,
                len: None,
            },
        )
        .await;
    assert!(matches!(
        refused,
        Err(ImError {
            kind: ErrorKind::Unsupported,
            ..
        })
    ));
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "不向平台发请求（只有连接时的核验）"
    );
}

#[tokio::test]
async fn stickers_are_read_from_the_image_cdn_whole_and_without_the_token() {
    let (server, client) = setup().await;
    // 取样（回溯 v59 §2-3）：同一编号在图片 CDN `…/static-resource/v1/<编号>~` 上，不带参数给原图；伪造编号 404。
    Mock::given(path("/static-resource/v1/v3_00t4_sticker~"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(b"GIF89a\x01\x00".to_vec(), "application/octet-stream"),
        )
        .mount(&server)
        .await;
    Mock::given(path("/static-resource/v1/v3_00t4_gone~"))
        .respond_with(ResponseTemplate::new(404).set_body_raw(b"{}".to_vec(), "application/json"))
        .mount(&server)
        .await;
    Mock::given(path("/static-resource/v1/v3_00t4_refused~"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let sticker = |key: &str| ResourceRef {
        message: message_ref(),
        key: key.into(),
        kind: ResourceKind::Sticker,
    };
    let opened = client
        .open_resource(&sticker("v3_00t4_sticker"), ResourceRange::default())
        .await
        .unwrap();
    assert_eq!(opened.total, Some(8));
    assert_eq!(read_all(opened).await, b"GIF89a\x01\x00");
    let last = server.received_requests().await.unwrap().pop().unwrap();
    assert!(
        last.headers.get("authorization").is_none(),
        "表情包不带令牌"
    );
    assert!(last.url.query().is_none(), "不带参数：原图，动图保留");
    let error = |key: &'static str, range: ResourceRange| {
        let client = &client;
        async move {
            client
                .open_resource(&sticker(key), range)
                .await
                .err()
                .map(|e| e.kind)
        }
    };
    assert_eq!(
        error("v3_00t4_gone", ResourceRange::default()).await,
        Some(ErrorKind::NotFound)
    );
    // CDN 的 401 是“这张读不到”，不是授权失效（否则会停下整组下载）。
    assert_eq!(
        error("v3_00t4_refused", ResourceRange::default()).await,
        Some(ErrorKind::NotFound)
    );
    let before = server.received_requests().await.unwrap().len();
    assert_eq!(
        error(
            "v3_00t4_sticker",
            ResourceRange {
                start: 4,
                len: None
            }
        )
        .await,
        Some(ErrorKind::Unsupported),
        "表情包整份读"
    );
    assert_eq!(
        error("file_v3_not_a_sticker", ResourceRange::default()).await,
        Some(ErrorKind::InvalidRequest)
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        before,
        "格式不对的不发请求"
    );
}

#[tokio::test]
async fn avatars_are_read_from_the_cdn_without_the_token() {
    let (server, client) = setup().await;
    Mock::given(path("/static-resource/v1/v3_00105_avatar~"))
        .and(query_param("image_size", "72x72"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(b"\x89PNG\r\n\x1a\n".to_vec(), "image/png"),
        )
        .mount(&server)
        .await;
    let url = format!(
        "{}/static-resource/v1/v3_00105_avatar~?image_size=72x72",
        server.uri()
    );
    let opened = client.open_avatar(&url).await.unwrap();
    assert_eq!(read_all(opened).await, b"\x89PNG\r\n\x1a\n");
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .last()
            .unwrap()
            .headers
            .get("authorization")
            .is_none(),
        "头像不带令牌"
    );
    for elsewhere in [
        "https://evil.example.com/static-resource/v1/v3_x~",
        "http://s1-imfile.feishucdn.com/static-resource/v1/v3_x~",
    ] {
        assert!(matches!(
            client.open_avatar(elsewhere).await,
            Err(ImError {
                kind: ErrorKind::InvalidRequest,
                ..
            })
        ));
    }
}

#[tokio::test]
async fn replacing_auth_cannot_change_verified_account() {
    let (server, mut client) = setup().await;
    Mock::given(path("/open-apis/authen/v1/user_info"))
        .and(header("authorization", "Bearer other"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"code":0,"data":{"open_id":"ou_other","tenant_key":"tenant"}}),
            ),
        )
        .mount(&server)
        .await;
    assert_eq!(
        client
            .replace_auth(AuthGrant::from_access_token("other").unwrap())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::PermissionDenied
    );
    assert_eq!(client.account().user.id, "ou_me");
    let caps = client.capabilities();
    assert_eq!(
        caps.capabilities
            .iter()
            .find(|c| c.name == "personalRealtime")
            .unwrap()
            .state,
        CapabilityState::Unsupported
    );
    assert_eq!(
        caps.capabilities
            .iter()
            .find(|c| c.name == "sendAsUser")
            .unwrap()
            .state,
        CapabilityState::Unknown
    );
    client
        .replace_auth(
            AuthGrant::from_parts(
                "user_token".into(),
                None,
                None,
                Some(vec!["im:message".into(), "im:message.send_as_user".into()]),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        client
            .capabilities()
            .capabilities
            .iter()
            .find(|c| c.name == "sendAsUser")
            .unwrap()
            .state,
        CapabilityState::Granted
    );
}

#[tokio::test]
async fn user_history_requires_base_and_chat_specific_permissions() {
    let (_server, mut client) = setup().await;
    for (scopes, group, direct) in [
        (
            vec!["im:message"],
            CapabilityState::MissingScope,
            CapabilityState::MissingScope,
        ),
        (
            vec!["im:message:readonly", "im:message.group_msg:get_as_user"],
            CapabilityState::Granted,
            CapabilityState::MissingScope,
        ),
        (
            vec!["im:message:readonly", "im:message.p2p_msg:get_as_user"],
            CapabilityState::MissingScope,
            CapabilityState::Granted,
        ),
    ] {
        client
            .replace_auth(
                AuthGrant::from_parts(
                    "user_token".into(),
                    None,
                    None,
                    Some(scopes.into_iter().map(str::to_owned).collect()),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let caps = client.capabilities();
        assert_eq!(
            caps.capabilities
                .iter()
                .find(|c| c.name == "readGroupHistory")
                .unwrap()
                .state,
            group
        );
        assert_eq!(
            caps.capabilities
                .iter()
                .find(|c| c.name == "readDirectHistory")
                .unwrap()
                .state,
            direct
        );
    }
}

fn login(auth: &FeishuAuth) -> (PendingLogin, String) {
    let pending = auth
        .begin_login(LoginRequest {
            redirect_uri: "http://127.0.0.1:1234/callback".into(),
            scopes: vec!["offline_access".into()],
        })
        .unwrap();
    let url = reqwest::Url::parse(pending.authorization_url()).unwrap();
    assert!(
        url.query_pairs()
            .any(|(k, v)| k == "response_type" && v == "code")
    );
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    (
        pending,
        format!("http://127.0.0.1:1234/callback?state={state}&code=one_use"),
    )
}

#[tokio::test]
async fn oauth_v3_exchange_and_refresh_return_rotated_credentials() {
    let server = MockServer::start().await;
    let auth = FeishuAuth::new(config(&server), "app".into(), "secret".into()).unwrap();
    Mock::given(path("/oauth/v3/token")).and(method("POST"))
        .and(body_json(json!({"grant_type":"authorization_code","client_id":"app","client_secret":"secret","code":"one_use","redirect_uri":"http://127.0.0.1:1234/callback"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code":0,"access_token":"access","refresh_token":"refresh","expires_in":10,"scope":"offline_access"}))).expect(1).mount(&server).await;
    let (pending, callback) = login(&auth);
    let grant = auth.finish_login(pending, &callback).await.unwrap();
    assert_eq!(grant.access_token(), "access");
    Mock::given(path("/oauth/v3/token")).and(body_json(json!({"grant_type":"refresh_token","client_id":"app","client_secret":"secret","refresh_token":"refresh"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"access_token":"new","refresh_token":"rotated","expires_in":20}))).expect(1).mount(&server).await;
    assert_eq!(
        auth.refresh_auth(&grant).await.unwrap().refresh_token(),
        Some("rotated")
    );
}

// 刷新失败按平台错误码分三类：必须重新授权、应用配置问题、临时故障；宿主据此决定是否打扰用户。
#[tokio::test]
async fn refresh_rejections_distinguish_reauthorization_from_transient_failures() {
    let cases = [
        (400, 20037, ErrorKind::AuthExpired),
        (400, 20064, ErrorKind::AuthExpired),
        (400, 20073, ErrorKind::AuthExpired),
        (400, 20026, ErrorKind::AuthExpired),
        (400, 20024, ErrorKind::AuthExpired),
        (400, 20074, ErrorKind::PermissionDenied),
        (400, 20002, ErrorKind::InvalidRequest),
        (500, 20050, ErrorKind::Disconnected),
        (503, 20072, ErrorKind::Disconnected),
    ];
    let grant = AuthGrant::from_parts("old".into(), Some("one_use".into()), Some(1), None).unwrap();
    for (status, code, kind) in cases {
        let server = MockServer::start().await;
        Mock::given(path("/oauth/v3/token"))
            .and(method("POST"))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                "code": code,
                "error": "invalid_grant",
                "error_description": "private_refresh_detail"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let auth = FeishuAuth::new(config(&server), "app".into(), "secret".into()).unwrap();
        let error = auth.refresh_auth(&grant).await.unwrap_err();
        assert_eq!(error.kind, kind, "错误码 {code}");
        assert_eq!(error.platform_code, Some(code.to_string()));
        assert!(!error.message.contains("private_refresh_detail"));
    }
}

#[tokio::test]
async fn invalid_oauth_callbacks_never_exchange_a_code() {
    let server = MockServer::start().await;
    let auth = FeishuAuth::new(config(&server), "app".into(), "secret".into()).unwrap();
    for change in 0..4 {
        let (pending, callback) = login(&auth);
        let callback = match change {
            0 => callback.replace("state=", "state=wrong"),
            1 => format!("{callback}&code=twice"),
            2 => callback.replace("/callback?", "/other?"),
            _ => callback.replace("code=one_use", "error=access_denied"),
        };
        assert!(auth.finish_login(pending, &callback).await.is_err());
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn oauth_failure_does_not_retry_or_echo_secrets() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("private_secret"))
        .expect(1)
        .mount(&server)
        .await;
    let auth = FeishuAuth::new(config(&server), "app".into(), "secret".into()).unwrap();
    let (pending, callback) = login(&auth);
    let error = auth.finish_login(pending, &callback).await.unwrap_err();
    assert!(!format!("{error:?}").contains("private_secret"));
}

fn recent_fixtures() -> (Value, Value) {
    (
        serde_json::from_str(include_str!("fixtures/recent_responses.json")).unwrap(),
        serde_json::from_str(include_str!("fixtures/recent_responses.expected.json")).unwrap(),
    )
}

// 2025-09-16T05:20:00Z：测试固定起点，请求体里的时间范围因此可精确匹配。
const RECENT_SINCE: i64 = 1_758_000_000;

async fn mock_recent_search(server: &MockServer, raw: &Value) {
    let body = json!({"filter":{"time_range":{"start_time":"2025-09-16T05:20:00Z"},"exclude_from_types":["bot"]}});
    Mock::given(method("POST"))
        .and(path("/open-apis/im/v1/messages/search"))
        .and(query_param("page_token", "p2"))
        .and(body_json(body.clone()))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":raw["page2"]})),
        )
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/open-apis/im/v1/messages/search"))
        .and(header("authorization", "Bearer user_token"))
        .and(query_param("user_id_type", "open_id"))
        .and(query_param("page_size", "30"))
        .and(wiremock::matchers::query_param_is_missing("page_token"))
        .and(body_json(body))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":raw["page1"]})),
        )
        .expect(1)
        .mount(server)
        .await;
}

async fn mock_recent_chats(server: &MockServer, raw: &Value) {
    for id in ["oc_team", "oc_weekend"] {
        Mock::given(method("GET"))
            .and(path(format!("/open-apis/im/v1/chats/{id}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":raw["chats"][id]})),
            )
            .expect(1)
            .mount(server)
            .await;
    }
}

#[tokio::test]
async fn recent_conversations_mix_groups_and_direct_chats_by_latest_message() {
    let (server, client) = setup().await;
    let (raw, expected) = recent_fixtures();
    mock_recent_search(&server, &raw).await;
    mock_recent_chats(&server, &raw).await;
    Mock::given(method("GET"))
        .and(path("/open-apis/contact/v3/users/batch"))
        .and(query_param("user_id_type", "open_id"))
        .and(query_param("user_ids", "ou_lin"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":raw["users"]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let recent = client
        .recent_conversations(&RecentQuery::new(10, RECENT_SINCE))
        .await
        .unwrap();
    assert_eq!(serde_json::to_value(recent).unwrap(), expected);
    // 身份、两页只读搜索、两个群资料和一次批量用户资料；没有发送或读取消息正文。
    assert_eq!(server.received_requests().await.unwrap().len(), 6);
}

// 会话发现（契约 hhh-im client v29 §2）：与最近聊天同一个不带关键词、排除机器人的搜索，翻到没有下一页为止；
// 按会话归并出最早、最新一条，按最新一条从新到旧；私聊对方取第一个不是本人的发送者，只有本人发过的为空；
// 不读群资料、不读通讯录（资料由调用方只给不在同步范围里的会话读）。
#[tokio::test]
async fn recent_activity_lists_every_chat_since_the_start_with_its_first_and_latest_message() {
    let (server, client) = setup().await;
    let (raw, _) = recent_fixtures();
    mock_recent_search(&server, &raw).await;
    let found = client
        .recent_activity(&ActivityQuery {
            since_ms: RECENT_SINCE * 1000 + 999,
            pages_max: 10,
        })
        .await
        .unwrap();
    assert!(found.complete, "翻到了没有下一页");
    let summary: Vec<(&str, ConversationKind, i64, i64, Option<&str>)> = found
        .items
        .iter()
        .map(|item| {
            (
                item.conversation.id.as_str(),
                item.kind.clone(),
                item.first_ms,
                item.latest_ms,
                item.peer.as_ref().map(|p| p.id.as_str()),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                "oc_team",
                ConversationKind::Group,
                1_790_216_400_000,
                1_790_217_000_000,
                None
            ),
            (
                "oc_dm_lin",
                ConversationKind::Direct,
                1_790_211_600_000,
                1_790_216_700_000,
                Some("ou_lin")
            ),
            (
                "oc_dm_self_only",
                ConversationKind::Direct,
                1_790_211_600_000,
                1_790_211_600_000,
                None
            ),
            (
                "oc_weekend",
                ConversationKind::Group,
                1_790_197_200_000,
                1_790_197_200_000,
                None
            ),
        ]
    );
    assert!(found.items.iter().all(|item| item.profile.is_none()));
    // 身份与两页只读搜索；没有读群资料、通讯录或消息正文。
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

// 翻满页数上限还没翻完：结果标为没翻完，不多发一页。
#[tokio::test]
async fn recent_activity_stops_at_the_page_limit_and_says_it_is_incomplete() {
    let (server, client) = setup().await;
    let (raw, _) = recent_fixtures();
    Mock::given(method("POST"))
        .and(path("/open-apis/im/v1/messages/search"))
        .and(wiremock::matchers::query_param_is_missing("page_token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":raw["page1"]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let found = client
        .recent_activity(&ActivityQuery {
            since_ms: RECENT_SINCE * 1000,
            pages_max: 1,
        })
        .await
        .unwrap();
    assert!(!found.complete);
    assert_eq!(found.items.len(), 2, "第一页里的两个会话");
}

// 不合法的输入与已知缺 search:message 的授权都在本地拒绝，不发请求。
#[tokio::test]
async fn recent_activity_refuses_bad_input_and_a_grant_without_search_before_network() {
    let (server, client) = setup().await;
    let bad = client
        .recent_activity(&ActivityQuery {
            since_ms: RECENT_SINCE * 1000,
            pages_max: 0,
        })
        .await
        .unwrap_err();
    assert_eq!(bad.kind, ErrorKind::InvalidRequest);
    let known = FeishuClient::connect(
        config(&server),
        "test",
        AuthGrant::from_parts(
            "user_token".into(),
            None,
            None,
            Some(vec!["im:chat:readonly".into()]),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let denied = known
        .recent_activity(&ActivityQuery {
            since_ms: RECENT_SINCE * 1000,
            pages_max: 1,
        })
        .await
        .unwrap_err();
    assert_eq!(denied.kind, ErrorKind::PermissionDenied);
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .all(|r| !r.url.path().contains("/messages/search")),
        "没有发搜索请求"
    );
}

#[tokio::test]
async fn recent_conversations_skip_direct_chats_without_contact_scope_and_explain() {
    let (server, _) = setup().await;
    let (raw, _) = recent_fixtures();
    mock_recent_search(&server, &raw).await;
    mock_recent_chats(&server, &raw).await;
    let known = FeishuClient::connect(
        config(&server),
        "test",
        AuthGrant::from_parts(
            "user_token".into(),
            None,
            None,
            Some(vec!["im:chat:readonly".into(), "search:message".into()]),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let recent = known
        .recent_conversations(&RecentQuery::new(10, RECENT_SINCE))
        .await
        .unwrap();
    let names: Vec<_> = recent
        .items
        .iter()
        .map(|item| item.conversation.name.as_str())
        .collect();
    assert_eq!(names, ["产品研发群", "周末去哪儿"], "只回群，私聊不猜名字");
    assert!(
        recent
            .notice
            .unwrap()
            .contains("contact:user.base:readonly")
    );
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .all(|request| !request.url.path().contains("/contact/")),
        "已知缺权限不请求通讯录"
    );
}

#[tokio::test]
async fn recent_conversations_treat_contact_denial_and_hidden_groups_as_skips() {
    let (server, client) = setup().await;
    let (raw, _) = recent_fixtures();
    mock_recent_search(&server, &raw).await;
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/chats/oc_team"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"code":232011})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/chats/oc_weekend"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":0,"data":raw["chats"]["oc_weekend"]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/open-apis/contact/v3/users/batch"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"code":99991672})))
        .expect(1)
        .mount(&server)
        .await;
    let recent = client
        .recent_conversations(&RecentQuery::new(10, RECENT_SINCE))
        .await
        .unwrap();
    let names: Vec<_> = recent
        .items
        .iter()
        .map(|item| item.conversation.name.as_str())
        .collect();
    assert_eq!(names, ["周末去哪儿"]);
    let notice = recent.notice.unwrap();
    assert!(
        notice.contains("contact:user.base:readonly") && notice.contains("部分群资料暂时无法读取")
    );
}

#[tokio::test]
async fn recent_conversations_bound_pages_and_reject_bad_input_before_network() {
    let (server, client) = setup().await;
    for bad in [
        RecentQuery::new(0, RECENT_SINCE),
        RecentQuery::new(21, RECENT_SINCE),
        RecentQuery::new(5, -1),
    ] {
        assert_eq!(
            client.recent_conversations(&bad).await.unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
    }
    let known = FeishuClient::connect(
        config(&server),
        "test",
        AuthGrant::from_parts(
            "user_token".into(),
            None,
            None,
            Some(vec!["im:chat:readonly".into()]),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let error = known
        .recent_conversations(&RecentQuery::new(5, RECENT_SINCE))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::PermissionDenied);
    assert!(error.message.contains("search:message"));
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        2,
        "只有两次身份请求"
    );

    // 平台一直说还有下一页，也最多读 3 页；每页都是同一个群，最后只回 1 个会话。
    let page = json!({"items":[{"meta_data":{"chat_id":"oc_team","from_id":"ou_a","is_p2p_chat":false,"create_time":"2026-09-24T10:20:00+08:00"}}],"has_more":true,"page_token":"more"});
    Mock::given(method("POST"))
        .and(path("/open-apis/im/v1/messages/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":page})))
        .expect(3)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/chats/oc_team"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":0,"data":{"name":"产品研发群","chat_mode":"group"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let recent = client
        .recent_conversations(&RecentQuery::new(5, RECENT_SINCE))
        .await
        .unwrap();
    assert_eq!(recent.items.len(), 1);
    assert!(recent.notice.is_none());
}

#[tokio::test]
async fn recent_conversations_reject_malformed_times_and_foreign_people() {
    let (server, client) = setup().await;
    let bad_time = json!({"items":[{"meta_data":{"chat_id":"oc_team","is_p2p_chat":false,"create_time":"yesterday"}}],"has_more":false});
    Mock::given(method("POST"))
        .and(path("/open-apis/im/v1/messages/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":bad_time})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    assert_eq!(
        client
            .recent_conversations(&RecentQuery::new(5, RECENT_SINCE))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::ProtocolError
    );
    let direct = json!({"items":[{"meta_data":{"chat_id":"oc_dm","from_id":"ou_lin","is_p2p_chat":true,"create_time":"2026-09-24T10:20:00+08:00"}}],"has_more":false});
    Mock::given(method("POST"))
        .and(path("/open-apis/im/v1/messages/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":direct})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/open-apis/contact/v3/users/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"code":0,"data":{"items":[{"open_id":"ou_stranger","name":"陌生人"}]}}),
        ))
        .mount(&server)
        .await;
    assert_eq!(
        client
            .recent_conversations(&RecentQuery::new(5, RECENT_SINCE))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::ProtocolError,
        "平台多返回的身份不能混进会话列表"
    );
}

fn user(id: &str) -> UserRef {
    UserRef {
        connection_id: "test".into(),
        id: id.into(),
    }
}

#[tokio::test]
async fn users_follow_request_order_skip_missing_and_prefer_small_avatars() {
    let (server, client) = setup().await;
    Mock::given(method("GET"))
        .and(path("/open-apis/contact/v3/users/batch"))
        .and(header("authorization", "Bearer user_token"))
        .and(query_param("user_id_type", "open_id"))
        .and(query_param("user_ids", "ou_b"))
        .and(query_param("user_ids", "ou_a"))
        .and(query_param("user_ids", "ou_gone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":{"items":[
            {"open_id":"ou_a","name":"阿甲","avatar":{"avatar_72":"https://x/a72.png","avatar_240":"https://x/a240.png"}},
            {"open_id":"ou_b","name":"阿乙","avatar":{"avatar_240":"https://x/b240.png"}}
        ]}})))
        .expect(1)
        .mount(&server)
        .await;
    let found = client
        .users(&[user("ou_b"), user("ou_a"), user("ou_b"), user("ou_gone")])
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(found).unwrap(),
        json!([
            {"reference":{"connectionId":"test","id":"ou_b"},"name":"阿乙","avatarUrl":"https://x/b240.png"},
            {"reference":{"connectionId":"test","id":"ou_a"},"name":"阿甲","avatarUrl":"https://x/a72.png"}
        ]),
        "按请求顺序、去重；平台没返回的人不出现"
    );
}

#[tokio::test]
async fn users_that_do_not_exist_are_left_out_instead_of_failing_the_batch() {
    // 真实取样（2026-09-30，契约 friend/im-watch v60）：一批里只要有一个编号不存在，飞书整批拒绝——
    // 400 + 99992351，`error.field_violations` 列出是哪个；格式不对的编号也是同一个错误码。
    let (server, client) = setup().await;
    Mock::given(method("GET"))
        .and(path("/open-apis/contact/v3/users/batch"))
        .respond_with(|request: &wiremock::Request| {
            let ids: Vec<String> = request
                .url
                .query_pairs()
                .filter(|(k, _)| k == "user_ids")
                .map(|(_, v)| v.into_owned())
                .collect();
            if let Some(gone) = ids.iter().find(|id| id.starts_with("ou_gone")) {
                return ResponseTemplate::new(400).set_body_json(json!({
                    "code": 99992351,
                    "msg": format!("The request you send is not a valid {{open_id}} or not exists. Invalid ids: [{gone}]"),
                    "error": {"field_violations": [{"field": "user_ids", "value": gone, "description": "id not exist"}]}
                }));
            }
            let items: Vec<Value> = ids
                .iter()
                .map(|id| json!({"open_id": id, "name": format!("名字-{id}")}))
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({"code": 0, "data": {"items": items}}))
        })
        .mount(&server)
        .await;
    let asked = [
        user("ou_a"),
        user("ou_gone1"),
        user("ou_b"),
        user("ou_c"),
        user("ou_gone2"),
    ];
    let found = client.users(&asked).await.unwrap();
    assert_eq!(
        found
            .iter()
            .map(|p| p.reference.id.as_str())
            .collect::<Vec<_>>(),
        ["ou_a", "ou_b", "ou_c"],
        "读得到的照请求顺序给出，读不到的不出现"
    );
    let batches = || async {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/open-apis/contact/v3/users/batch")
            .count()
    };
    // 被拒的一批对半分开再读：5 人里 2 个读不到，1 + 2 + 2 + 2 + 2 = 9 次。
    assert_eq!(batches().await, 9);
    // 一批 40 人里 1 个读不到：约 1 + 2·log₂40 次，远少于一个一个读。
    let before = batches().await;
    let mut crowd: Vec<_> = (0..39).map(|n| user(&format!("ou_{n}"))).collect();
    crowd.insert(17, user("ou_gone_in_crowd"));
    assert_eq!(client.users(&crowd).await.unwrap().len(), 39);
    assert!(
        batches().await - before <= 13,
        "请求次数 {}",
        batches().await - before
    );
    // 全都不存在时得到空结果，不是错误。
    assert!(client.users(&[user("ou_gone3")]).await.unwrap().is_empty());
}

#[tokio::test]
async fn users_from_another_app_do_not_hide_other_peoples_avatars() {
    // 2026-10-20 只读取样：单人可读、混合批次被 400 + 99992361（open_id cross app）拒绝。
    // 编号与资料均已替换；这类单人错误只能影响本人，不能丢掉同批的正常头像。
    let (server, client) = setup().await;
    Mock::given(method("GET"))
        .and(path("/open-apis/contact/v3/users/batch"))
        .respond_with(|request: &wiremock::Request| {
            let ids: Vec<String> = request
                .url
                .query_pairs()
                .filter(|(k, _)| k == "user_ids")
                .map(|(_, v)| v.into_owned())
                .collect();
            if ids.iter().any(|id| id.starts_with("ou_foreign")) {
                return ResponseTemplate::new(400)
                    .set_body_json(json!({"code":99992361,"msg":"open_id cross app"}));
            }
            let items: Vec<Value> = ids
                .iter()
                .map(|id| {
                    json!({
                        "open_id":id,"name":format!("名字-{id}"),
                        "avatar":{"avatar_72":format!("https://s1-imfile.feishucdn.com/{id}.png")}
                    })
                })
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":{"items":items}}))
        })
        .mount(&server)
        .await;
    let mut asked: Vec<_> = (0..52).map(|n| user(&format!("ou_good{n}"))).collect();
    asked.insert(0, user("ou_foreign_first"));
    asked.insert(25, user("ou_foreign_middle"));
    asked.push(user("ou_foreign_last"));
    asked.push(user("ou_good0"));
    let found = client.users(&asked).await.unwrap();
    assert_eq!(
        found.len(),
        52,
        "跨过 50 人的批次边界，正常人员全部保留、重复人员只给一次"
    );
    for (n, person) in found.iter().enumerate() {
        assert_eq!(person.reference.id, format!("ou_good{n}"));
        assert_eq!(person.name, format!("名字-ou_good{n}"));
        assert_eq!(
            person.avatar_url,
            Some(format!("https://s1-imfile.feishucdn.com/ou_good{n}.png"))
        );
    }
    assert!(
        client
            .users(&[user("ou_foreign_only")])
            .await
            .unwrap()
            .is_empty()
    );
    let requests = server.received_requests().await.unwrap();
    let batches = requests
        .iter()
        .filter(|r| r.url.path() == "/open-apis/contact/v3/users/batch")
        .count();
    assert!(
        batches < 40,
        "只有失败子批次拆开，正常批次不逐人读取：{batches}"
    );
}

#[tokio::test]
async fn account_wide_and_unknown_people_errors_are_not_split_into_individual_requests() {
    for (status, code, kind) in [
        (400, 99991668, ErrorKind::AuthExpired),
        (400, 99991672, ErrorKind::PermissionDenied),
        (429, 99991400, ErrorKind::RateLimited),
        (503, -1, ErrorKind::Disconnected),
        (400, 12345678, ErrorKind::ProtocolError),
    ] {
        let (server, client) = setup().await;
        Mock::given(method("GET"))
            .and(path("/open-apis/contact/v3/users/batch"))
            .respond_with(
                ResponseTemplate::new(status).set_body_json(json!({"code":code,"msg":"failed"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let error = client
            .users(&[user("ou_a"), user("ou_b")])
            .await
            .unwrap_err();
        assert_eq!(error.kind, kind);
        assert_eq!(error.platform_code, Some(code.to_string()));
        server.verify().await;
    }
}

#[tokio::test]
async fn users_split_large_batches_and_reject_bad_input_before_network() {
    let (server, client) = setup().await;
    Mock::given(method("GET"))
        .and(path("/open-apis/contact/v3/users/batch"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":{"items":[]}})),
        )
        .expect(2)
        .mount(&server)
        .await;
    let many: Vec<_> = (0..51).map(|n| user(&format!("ou_{n}"))).collect();
    assert!(client.users(&many).await.unwrap().is_empty());
    assert!(client.users(&[]).await.unwrap().is_empty());
    let too_many: Vec<_> = (0..501).map(|n| user(&format!("ou_{n}"))).collect();
    assert_eq!(
        client.users(&too_many).await.unwrap_err().kind,
        ErrorKind::InvalidRequest
    );
    let foreign = UserRef {
        connection_id: "other".into(),
        id: "ou_a".into(),
    };
    assert_eq!(
        client.users(&[foreign]).await.unwrap_err().kind,
        ErrorKind::InvalidRequest
    );
    // 身份核验 + 51 人分两批；空列表与非法输入都不发请求。
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn users_without_contact_scope_explain_reauthorization_without_network() {
    let (server, _) = setup().await;
    let known = FeishuClient::connect(
        config(&server),
        "test",
        AuthGrant::from_parts(
            "user_token".into(),
            None,
            None,
            Some(vec!["im:chat:readonly".into()]),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let error = known.users(&[user("ou_a")]).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::PermissionDenied);
    assert!(error.message.contains("contact:user.base:readonly"));
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .all(|r| r.url.path() != "/open-apis/contact/v3/users/batch"),
        "已知缺权限时不请求通讯录"
    );
}

fn direct_message(id: &str, sender: &str, sender_type: &str) -> Value {
    json!({
        "message_id": id, "chat_id": "oc_dm", "msg_type": "text",
        "create_time": "1700000001000",
        "sender": {"id": sender, "id_type": "open_id", "sender_type": sender_type},
        "body": {"content": "{\"text\":\"hi\"}"}
    })
}

async fn mock_direct_page(server: &MockServer, items: Vec<Value>) {
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages"))
        .and(query_param("container_id_type", "chat"))
        .and(query_param("container_id", "oc_dm"))
        .and(query_param("page_size", "20"))
        .and(query_param("sort_type", "ByCreateTimeDesc"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":0,"data":{"items":items,"has_more":false}})),
        )
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn direct_peer_is_the_only_other_user_in_the_latest_page() {
    let (server, client) = setup().await;
    mock_direct_page(
        &server,
        vec![
            direct_message("om_1", "ou_me", "user"),
            direct_message("om_2", "cli_bot", "app"),
            direct_message("om_3", "ou_lin", "user"),
            direct_message("om_4", "ou_lin", "user"),
        ],
    )
    .await;
    let peer = client
        .find_direct_peer(&ConversationRef::new("test", "oc_dm"))
        .await
        .unwrap();
    assert_eq!(peer, Some(user("ou_lin")));
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests.iter().all(|r| r.method.as_str() == "GET"),
        "认人只读消息，不发送"
    );
}

#[tokio::test]
async fn direct_peer_stays_unknown_with_only_own_messages_and_rejects_group_like_pages() {
    let (server, client) = setup().await;
    mock_direct_page(&server, vec![direct_message("om_1", "ou_me", "user")]).await;
    assert_eq!(
        client
            .find_direct_peer(&ConversationRef::new("test", "oc_dm"))
            .await
            .unwrap(),
        None
    );
    let (server, client) = setup().await;
    mock_direct_page(
        &server,
        vec![
            direct_message("om_1", "ou_a", "user"),
            direct_message("om_2", "ou_b", "user"),
        ],
    )
    .await;
    assert_eq!(
        client
            .find_direct_peer(&ConversationRef::new("test", "oc_dm"))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::ProtocolError
    );
    assert_eq!(
        client
            .find_direct_peer(&ConversationRef::new("other", "oc_dm"))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidRequest
    );
}

#[tokio::test]
async fn members_page_through_the_chat_with_the_user_token_and_skip_nothing() {
    let (server, client) = setup().await;
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/chats/oc_group/members"))
        .and(header("authorization", "Bearer user_token"))
        .and(query_param("member_id_type", "open_id"))
        .and(query_param("page_size", "2"))
        .and(query_param("page_token", "next_page"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":{
                "items":[{"member_id":"ou_c","name":"丙","member_id_type":"open_id"}],
                "has_more":false
            }})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/chats/oc_group/members"))
        .and(header("authorization", "Bearer user_token"))
        .and(query_param("member_id_type", "open_id"))
        .and(query_param("page_size", "2"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":{
                "items":[
                    {"member_id":"ou_a","name":"甲","member_id_type":"open_id"},
                    {"member_id":"ou_me","name":"Alice","member_id_type":"open_id"}
                ],
                "has_more":true,
                "page_token":"next_page"
            }})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // 第一页：编号与名字按平台顺序原样带回（包括本人），头像留空；还有下一页。
    let first = client.list_members(&chat(), 2, None).await.unwrap();
    let names: Vec<_> = first
        .items
        .iter()
        .map(|m| (m.reference.id.as_str(), m.name.as_str()))
        .collect();
    assert_eq!(names, [("ou_a", "甲"), ("ou_me", "Alice")]);
    assert!(first.items.iter().all(|m| m.avatar_url.is_none()));
    assert!(
        first
            .items
            .iter()
            .all(|m| m.reference.connection_id == "test")
    );
    // 第二页带上一页的游标；读完后不再有下一页。
    let second = client
        .list_members(&chat(), 2, first.next.as_ref())
        .await
        .unwrap();
    assert_eq!(second.items[0].reference.id, "ou_c");
    assert!(second.next.is_none());

    // 页大小越界、别的连接的会话、别的查询范围的游标：本地就拒绝，不发请求。
    let before = server.received_requests().await.unwrap().len();
    for limit in [0, MEMBER_PAGE_MAX + 1] {
        assert_eq!(
            client
                .list_members(&chat(), limit, None)
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidRequest
        );
    }
    assert!(
        client
            .list_members(&ConversationRef::new("other", "oc_group"), 2, None)
            .await
            .is_err()
    );
    let foreign = client.list_members(&chat(), 3, first.next.as_ref()).await;
    assert!(
        foreign.is_err(),
        "游标属于 page_size=2 的查询，不能拿去翻 3 条一页的"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), before);
}

#[tokio::test]
async fn members_missing_a_name_are_a_protocol_error() {
    let (server, client) = setup().await;
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/chats/oc_group/members"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":{
                "items":[{"member_id":"ou_a"}],
                "has_more":false
            }})),
        )
        .mount(&server)
        .await;
    assert_eq!(
        client
            .list_members(&chat(), 10, None)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::ProtocolError
    );
}

#[tokio::test]
async fn uploads_send_images_and_files_as_multipart_and_return_the_platform_key() {
    let (server, client) = setup().await;
    Mock::given(method("POST"))
        .and(path("/open-apis/im/v1/images"))
        .and(header("authorization", "Bearer user_token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":0,"data":{"image_key":"img_v3_upload"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/open-apis/im/v1/files"))
        .and(header("authorization", "Bearer user_token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":0,"data":{"file_key":"file_v3_upload"}})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let image = client
        .upload_resource(ResourceKind::Image, "截图.png", b"PNG-BYTES".to_vec())
        .await
        .unwrap();
    assert_eq!(image, "img_v3_upload");
    let file = client
        .upload_resource(ResourceKind::File, "周报.pdf", b"PDF-BYTES".to_vec())
        .await
        .unwrap();
    assert_eq!(file, "file_v3_upload");

    // 表单里：图片带 image_type=message；文件带平台文件种类与原文件名；字节原样上传。
    let uploads: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "POST")
        .collect();
    let image_body = String::from_utf8(uploads[0].body.clone()).unwrap();
    assert!(
        uploads[0].headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("multipart/form-data; boundary=")
    );
    assert!(image_body.contains("name=\"image_type\""));
    assert!(image_body.contains("message"));
    assert!(image_body.contains("name=\"image\"; filename=\"截图.png\""));
    assert!(image_body.contains("PNG-BYTES"));
    let file_body = String::from_utf8(uploads[1].body.clone()).unwrap();
    assert!(file_body.contains("name=\"file_type\""));
    assert!(file_body.contains("pdf"));
    assert!(file_body.contains("name=\"file_name\""));
    assert!(file_body.contains("name=\"file\"; filename=\"周报.pdf\""));
    assert!(file_body.contains("PDF-BYTES"));
}

#[tokio::test]
async fn uploads_reject_bad_input_or_a_grant_without_resource_scope_before_network() {
    let (server, client) = setup().await;
    let before = server.received_requests().await.unwrap().len();
    // 空内容、超过上限：LimitExceeded；空名字、带路径符号：InvalidRequest。
    for (kind, name, bytes) in [
        (ResourceKind::Image, "a.png", Vec::new()),
        (
            ResourceKind::Image,
            "a.png",
            vec![0; IMAGE_UPLOAD_MAX_BYTES + 1],
        ),
        (
            ResourceKind::File,
            "a.bin",
            vec![0; FILE_UPLOAD_MAX_BYTES + 1],
        ),
    ] {
        assert_eq!(
            client
                .upload_resource(kind, name, bytes)
                .await
                .unwrap_err()
                .kind,
            ErrorKind::LimitExceeded
        );
    }
    for name in [" ", "../a.png", "dir/a.png"] {
        assert_eq!(
            client
                .upload_resource(ResourceKind::File, name, b"x".to_vec())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidRequest
        );
    }
    assert_eq!(server.received_requests().await.unwrap().len(), before);

    // 已知授权范围里没有资源权限：提示重新授权，不发上传请求。
    let limited = FeishuClient::connect(
        config(&server),
        "test",
        AuthGrant::from_parts(
            "user_token".into(),
            None,
            None,
            Some(vec!["im:message".into()]),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let before = server.received_requests().await.unwrap().len();
    let error = limited
        .upload_resource(ResourceKind::Image, "a.png", b"PNG".to_vec())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::PermissionDenied);
    assert!(error.message.contains("重新登录"));
    assert_eq!(server.received_requests().await.unwrap().len(), before);
}

#[tokio::test]
async fn recall_checks_the_message_then_deletes_it_with_the_user_token() {
    let (server, client) = setup().await;
    mock_reaction_message(&server).await;
    Mock::given(method("DELETE"))
        .and(path("/open-apis/im/v1/messages/om_reply"))
        .and(header("authorization", "Bearer user_token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code":0,"msg":"success","data":{}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    client.recall(&message_ref()).await.unwrap();
    // 别的连接的消息：本地就拒绝，不发请求。
    let before = server.received_requests().await.unwrap().len();
    let foreign = MessageRef {
        conversation: ConversationRef::new("other", "oc_group"),
        id: "om_reply".into(),
    };
    assert!(client.recall(&foreign).await.is_err());
    assert_eq!(server.received_requests().await.unwrap().len(), before);
}

#[tokio::test]
async fn recall_refusals_keep_the_platform_reason() {
    let cases = [
        (
            230009,
            ErrorKind::LimitExceeded,
            "消息发出时间过长，不能撤回。",
        ),
        (
            230026,
            ErrorKind::PermissionDenied,
            "只能撤回自己发出的消息。",
        ),
        (230110, ErrorKind::NotFound, "消息已被删除，无法撤回。"),
    ];
    for (code, kind, text) in cases {
        let (server, client) = setup().await;
        mock_reaction_message(&server).await;
        Mock::given(method("DELETE"))
            .and(path("/open-apis/im/v1/messages/om_reply"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(json!({"code":code,"msg":"private detail"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let error = client.recall(&message_ref()).await.unwrap_err();
        assert_eq!(error.kind, kind, "错误码 {code}");
        assert_eq!(error.message, text);
        assert_eq!(error.platform_code, Some(code.to_string()));
    }
}

#[tokio::test]
async fn thread_reads_cannot_carry_a_time_range() {
    let (server, client) = setup().await;
    let before = server.received_requests().await.unwrap().len();
    let mut query = MessageQuery::recent(chat());
    query.thread_id = Some("omt_1".into());
    query.start_time = Some(1);
    assert_eq!(
        client.list_messages(&query).await.unwrap_err().kind,
        ErrorKind::InvalidRequest
    );
    assert_eq!(server.received_requests().await.unwrap().len(), before);
}

#[tokio::test]
async fn reactions_are_added_and_removed_with_the_user_token_and_official_bodies() {
    let (server, client) = setup().await;
    mock_reaction_message(&server).await;
    let reaction = json!({
        // 飞书真实的表情操作编号是 URL 安全的 base64，末尾带“=”补位（2026-09-25 真实环境里取消时曾被本地校验拒绝）。
        "reaction_id": "ZCaCIjUBVVWSrm5L-3ZTw_Qa==",
        "operator": {"operator_id": "ou_me", "operator_type": "user"},
        "action_time": "1690000000000",
        "reaction_type": {"emoji_type": "THUMBSUP"}
    });
    Mock::given(method("POST"))
        .and(path("/open-apis/im/v1/messages/om_reply/reactions"))
        .and(header("authorization", "Bearer user_token"))
        .and(body_json(
            json!({"reaction_type": {"emoji_type": "THUMBSUP"}}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":0,"msg":"success","data":reaction})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(
            "/open-apis/im/v1/messages/om_reply/reactions/ZCaCIjUBVVWSrm5L-3ZTw_Qa==",
        ))
        .and(header("authorization", "Bearer user_token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":0,"msg":"success","data":reaction})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let added = client
        .add_reaction(&message_ref(), "THUMBSUP")
        .await
        .unwrap();
    assert_eq!(added.id, "ZCaCIjUBVVWSrm5L-3ZTw_Qa==");
    assert_eq!(added.emoji_type, "THUMBSUP");
    assert_eq!(added.operator.id, "ou_me");
    assert_eq!(added.action_time_ms, Some(1_690_000_000_000));
    assert_eq!(added.message, message_ref());
    client
        .remove_reaction(&message_ref(), &added.id)
        .await
        .unwrap();
    // 格式不对的表情类型、表情操作编号与别的连接的消息：本地就拒绝，不发请求。
    let before = server.received_requests().await.unwrap().len();
    for bad in ["", "a/b", "a b", "a?b", "a%2Fb"] {
        assert_eq!(
            client
                .remove_reaction(&message_ref(), bad)
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidRequest,
            "{bad}"
        );
    }
    assert_eq!(
        client
            .add_reaction(&message_ref(), "OK THEN")
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidRequest
    );
    let foreign = MessageRef {
        conversation: ConversationRef::new("other", "oc_group"),
        id: "om_reply".into(),
    };
    assert!(client.add_reaction(&foreign, "OK").await.is_err());
    assert_eq!(server.received_requests().await.unwrap().len(), before);
}

#[tokio::test]
async fn reaction_refusals_explain_themselves_in_reaction_terms() {
    let cases = [
        (
            231003,
            ErrorKind::NotFound,
            "消息已撤回或不存在，不能点表情。",
        ),
        (
            231002,
            ErrorKind::PermissionDenied,
            "你不在这个会话里，或没有权限给这条消息点表情。",
        ),
        (231017, ErrorKind::Unsupported, "这类消息不能点表情。"),
        (231021, ErrorKind::PermissionDenied, "外部群里不能点表情。"),
    ];
    for (code, kind, text) in cases {
        let (server, client) = setup().await;
        mock_reaction_message(&server).await;
        Mock::given(method("POST"))
            .and(path("/open-apis/im/v1/messages/om_reply/reactions"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(json!({"code":code,"msg":"private detail"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let error = client.add_reaction(&message_ref(), "OK").await.unwrap_err();
        assert_eq!(error.kind, kind, "错误码 {code}");
        assert_eq!(error.message, text);
        assert_eq!(error.platform_code, Some(code.to_string()));
    }
}

#[tokio::test]
async fn the_emoji_catalog_comes_from_the_open_platform_document_without_the_user_token() {
    let (server, client) = setup().await;
    let base = server.uri();
    let content = format!(
        "# 表情文案说明\n<md-table><md-tbody><md-tr><md-td>![OK]({base}/img/ok.png?height=96&width=104)</md-td><md-td>OK</md-td><md-td>![THUMBSUP]({base}/img/thumbsup.png?height=96)</md-td><md-td>THUMBSUP</md-td></md-tr></md-tbody></md-table>"
    );
    Mock::given(method("GET"))
        .and(path("/document_portal/v1/document/get_detail"))
        .and(query_param("fullPath", "/server-docs/im-v1/message-reaction/emojis-introduce"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"code":0,"msg":"succeeded","data":{"updateTime":1739946865000_i64,"content":content}}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/img/ok.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3,
        ]))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/img/thumbsup.png"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>not found</html>"))
        .mount(&server)
        .await;
    let catalog = client.emoji_catalog().await.unwrap();
    assert_eq!(catalog.source_updated_ms, 1_739_946_865_000);
    let types: Vec<&str> = catalog
        .emojis
        .iter()
        .map(|e| e.emoji_type.as_str())
        .collect();
    assert_eq!(types, ["OK", "THUMBSUP"]);
    assert_eq!(catalog.emojis[0].image_url, format!("{base}/img/ok.png"));
    // 文档站是公开内容：请求不带用户令牌。
    let requests = server.received_requests().await.unwrap();
    let doc = requests
        .iter()
        .find(|r| r.url.path() == "/document_portal/v1/document/get_detail")
        .unwrap();
    assert!(doc.headers.get("authorization").is_none());
    // 图片：真的图片才收下；不是图片、不在飞书 CDN 的都拒绝（后者不发请求）。
    assert_eq!(
        client.emoji_image(&catalog.emojis[0]).await.unwrap().len(),
        11
    );
    assert_eq!(
        client
            .emoji_image(&catalog.emojis[1])
            .await
            .unwrap_err()
            .kind,
        ErrorKind::ProtocolError
    );
    let before = server.received_requests().await.unwrap().len();
    let foreign = EmojiInfo::new("OK", "https://example.com/ok.png");
    assert!(client.emoji_image(&foreign).await.is_err());
    assert_eq!(server.received_requests().await.unwrap().len(), before);
}

/// 一条卡片消息的原始响应：内容换成给定的卡片 JSON。
fn card_message(id: &str, content: Value) -> Value {
    let mut raw = raw_message();
    raw["message_id"] = id.into();
    raw["msg_type"] = "interactive".into();
    raw["body"]["content"] = content.to_string().into();
    raw.as_object_mut().unwrap().remove("mentions");
    raw
}

#[tokio::test]
async fn cards_are_read_as_raw_json_and_unknown_ones_fall_back_to_simplified_content() {
    let (server, client) = setup().await;
    let raw_card = card_message(
        "om_raw",
        json!({"header": {"template": "blue", "title": {"content": "PR Review 汇总", "tag": "plain_text"}},
            "elements": [{"tag": "div", "text": {"content": "**238**\n当前 PR", "tag": "lark_md"}}]}),
    );
    // 还没取样过的结构（如以后的新 schema）：认不出，改读简化内容。卡片 2.0 已按原始结构读（client v16）。
    let unknown: Vec<Value> = (1..=4)
        .map(|i| card_message(&format!("om_new_{i}"), json!({"schema": "3.0", "body": {}})))
        .collect();
    let mut items = vec![raw_card];
    items.extend(unknown);
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages"))
        .and(query_param("card_msg_content_type", "user_card_content"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": 0, "data": {"items": items, "has_more": false}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let simplified = |id: &str| {
        card_message(
            id,
            json!({"title": "新版卡片", "elements": [[{"tag": "text", "text": "简化内容"}]]}),
        )
    };
    for id in ["om_new_1", "om_new_2"] {
        Mock::given(method("GET"))
            .and(path(format!("/open-apis/im/v1/messages/{id}")))
            .and(wiremock::matchers::query_param_is_missing(
                "card_msg_content_type",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"code": 0, "data": {"items": [simplified(id)]}})),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    // 补读失败的那条保持暂不支持，不影响这一页。
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages/om_new_3"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;
    let page = client
        .list_messages(&MessageQuery::recent(chat()))
        .await
        .unwrap();
    let kinds: Vec<(&str, bool, bool)> = page
        .items
        .iter()
        .map(|m| {
            (
                m.reference.id.as_str(),
                matches!(m.content.as_slice(), [ImContent::Card(_)]),
                m.incomplete,
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("om_raw", true, false),
            ("om_new_1", true, true),
            ("om_new_2", true, true),
            ("om_new_3", false, true),
            // 每页至多补读 3 条，第 4 条保持暂不支持。
            ("om_new_4", false, true),
        ]
    );
    let requests = server.received_requests().await.unwrap();
    assert!(!requests.iter().any(|r| r.url.path().ends_with("om_new_4")));
}

/// 契约 hhh-im client v16 §1：卡片 2.0 按读回的原始结构翻译，一页只读一次、不为它补读简化内容。
#[tokio::test]
async fn v2_cards_are_read_from_raw_json_without_falling_back() {
    let (server, client) = setup().await;
    let v2 = card_message(
        "om_v2",
        json!({"schema": "2.0", "config": {"width_mode": "fill"},
            "header": {"template": "blue", "title": {"content": "日报", "tag": "plain_text"},
                "subtitle": {"content": "日期：2026-09-29", "tag": "plain_text"}},
            "body": {"elements": [{"tag": "markdown", "content": "## 稳定性总览
相当于 <font color='green'>**0.9折**</font>"}]}}),
    );
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages"))
        .and(query_param("card_msg_content_type", "user_card_content"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": 0, "data": {"items": [v2], "has_more": false}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let page = client
        .list_messages(&MessageQuery::recent(chat()))
        .await
        .unwrap();
    let [message] = page.items.as_slice() else {
        panic!("{:?}", page.items)
    };
    assert!(!message.incomplete);
    let [ImContent::Card(card)] = message.content.as_slice() else {
        panic!("{:?}", message.content)
    };
    assert_eq!(card.subtitle.as_deref(), Some("日期：2026-09-29"));
    assert!(card.wide);
    assert!(matches!(card.blocks.as_slice(), [CardBlock::Rich { .. }]));
    let requests = server.received_requests().await.unwrap();
    assert!(
        !requests.iter().any(|r| r.url.path().ends_with("/om_v2")),
        "没有补读简化内容"
    );
}

/// 取样卡 A 读回的原始卡片（user_card_content：四张 Markdown 表格，对齐被飞书规整掉了）。
fn tables_card(id: &str) -> Value {
    let item: Value =
        serde_json::from_str(include_str!("fixtures/feishu/response_card_v2_tables.json")).unwrap();
    let content: Value = serde_json::from_str(item["body"]["content"].as_str().unwrap()).unwrap();
    card_message(id, content)
}

/// 同一张卡片的 raw_card_content 读回（飞书内部结构 json_card），消息编号换成 `id`。
fn tables_card_structure(id: &str) -> Value {
    let mut raw: Value =
        serde_json::from_str(include_str!("fixtures/feishu/raw_card/tables.json")).unwrap();
    raw["data"]["items"][0]["message_id"] = id.into();
    raw
}

fn table_aligns(message: &ImMessage) -> Vec<Vec<Option<RichAlign>>> {
    let mut out = Vec::new();
    for content in &message.content {
        if let ImContent::Card(card) = content {
            card.walk_rich(&mut |node| {
                if let RichNode::Table { align, .. } = node {
                    out.push(align.clone());
                }
            });
        }
    }
    out
}

fn raw_card_reads(requests: &[wiremock::Request]) -> usize {
    requests
        .iter()
        .filter(|r| {
            r.url
                .query_pairs()
                .any(|(k, v)| k == "card_msg_content_type" && v == "raw_card_content")
        })
        .count()
}

/// 契约 hhh-im client v19 §2：卡片 2.0 的 Markdown 表格读回时丢了对齐，按 raw_card_content 的内部结构补上；
/// 同一张卡片记住，复核窗口里再读到时不再请求。
#[tokio::test]
async fn card_tables_take_their_alignment_from_the_card_structure_once() {
    let (server, client) = setup().await;
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages"))
        .and(query_param("card_msg_content_type", "user_card_content"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"code": 0, "data": {"items": [tables_card("om_tables")], "has_more": false}}),
        ))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages/om_tables"))
        .and(query_param("card_msg_content_type", "raw_card_content"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tables_card_structure("om_tables")))
        .expect(1)
        .mount(&server)
        .await;
    for _ in 0..2 {
        let page = client
            .list_messages(&MessageQuery::recent(chat()))
            .await
            .unwrap();
        let [message] = page.items.as_slice() else {
            panic!("{:?}", page.items)
        };
        let aligns = table_aligns(message);
        assert_eq!(aligns.len(), 4);
        assert_eq!(
            aligns[2],
            [
                Some(RichAlign::Left),
                Some(RichAlign::Center),
                Some(RichAlign::Right)
            ]
        );
        assert!(aligns[0].iter().all(|a| *a == Some(RichAlign::Left)));
    }
}

/// 契约 hhh-im client v19 §2：一页至多当场补读 5 条；补读失败的卡片照常显示、表格没标对齐。
#[tokio::test]
async fn table_alignment_reads_are_capped_per_page_and_failures_keep_the_card() {
    let (server, client) = setup().await;
    let items: Vec<Value> = (1..=7).map(|i| tables_card(&format!("om_t{i}"))).collect();
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages"))
        .and(query_param("card_msg_content_type", "user_card_content"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": 0, "data": {"items": items, "has_more": false}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(query_param("card_msg_content_type", "raw_card_content"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let page = client
        .list_messages(&MessageQuery::recent(chat()))
        .await
        .unwrap();
    assert_eq!(page.items.len(), 7);
    for message in &page.items {
        assert!(!message.incomplete);
        assert!(
            table_aligns(message)
                .iter()
                .all(|align| align.iter().all(Option::is_none))
        );
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(raw_card_reads(&requests), 5, "每页至多补读 5 条");
}

/// 契约 hhh-im client v19 §2、§3：按编号补读一条（视野里的重读也走这里）时同样补上对齐。
#[tokio::test]
async fn reading_one_card_fills_its_table_alignment() {
    let (server, client) = setup().await;
    let reference = MessageRef {
        conversation: chat(),
        id: "om_tables".into(),
    };
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages/om_tables"))
        .and(query_param("card_msg_content_type", "user_card_content"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": 0, "data": {"items": [tables_card("om_tables")]}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages/om_tables"))
        .and(query_param("card_msg_content_type", "raw_card_content"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tables_card_structure("om_tables")))
        .expect(1)
        .mount(&server)
        .await;
    let message = client.get_message(&reference).await.unwrap();
    assert_eq!(table_aligns(&message)[2][2], Some(RichAlign::Right));
    // 内部结构不是这条消息的：不用，照原样。
    let other = MessageRef {
        conversation: chat(),
        id: "om_other".into(),
    };
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages/om_other"))
        .and(query_param("card_msg_content_type", "user_card_content"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": 0, "data": {"items": [tables_card("om_other")]}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages/om_other"))
        .and(query_param("card_msg_content_type", "raw_card_content"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tables_card_structure("om_tables")))
        .mount(&server)
        .await;
    let message = client.get_message(&other).await.unwrap();
    assert!(
        table_aligns(&message)
            .iter()
            .all(|align| align.iter().all(Option::is_none))
    );
}

/// 取样卡（契约 hhh-im client v31）：发布机器人的卡片 1.0，`lark_md` 里的 @ 是发卡片那个应用的编号；消息带提及名单。
/// 编号与名字换成假的，会话换成测试会话，消息编号换成 `id`。
fn at_card(id: &str) -> Value {
    let mut item: Value =
        serde_json::from_str(include_str!("fixtures/feishu/response_card_at.json")).unwrap();
    item["message_id"] = id.into();
    item["chat_id"] = "oc_group".into();
    item
}

/// 同一张卡片的 raw_card_content 读回（`json_attachment.at_users` 把卡片里的编号对到提及名单的 key），消息编号换成 `id`。
fn at_card_structure(id: &str) -> Value {
    let mut raw: Value =
        serde_json::from_str(include_str!("fixtures/feishu/raw_card/at.json")).unwrap();
    raw["data"]["items"][0]["message_id"] = id.into();
    raw
}

fn card_mention_actors(message: &ImMessage) -> Vec<ActorRef> {
    let mut out = Vec::new();
    for content in &message.content {
        if let ImContent::Card(card) = content {
            card.walk_rich(&mut |node| {
                if let RichNode::Mention {
                    actor: Some(actor), ..
                } = node
                {
                    out.push(actor.clone());
                }
            });
        }
    }
    out
}

/// 契约 hhh-im client v31 §2 第 3、4 条：卡片里的 @ 对不上提及名单时按 raw_card_content 对到能认的人；
/// 同一张卡片记住，复核窗口里再读到时不再请求。
#[tokio::test]
async fn card_mentions_from_another_app_are_matched_once_through_the_card_structure() {
    let (server, client) = setup().await;
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages"))
        .and(query_param("card_msg_content_type", "user_card_content"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"code": 0, "data": {"items": [at_card("om_at")], "has_more": false}}),
        ))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages/om_at"))
        .and(query_param("card_msg_content_type", "raw_card_content"))
        .respond_with(ResponseTemplate::new(200).set_body_json(at_card_structure("om_at")))
        .expect(1)
        .mount(&server)
        .await;
    for _ in 0..2 {
        let page = client
            .list_messages(&MessageQuery::recent(chat()))
            .await
            .unwrap();
        let [message] = page.items.as_slice() else {
            panic!("{:?}", page.items)
        };
        let actors = card_mention_actors(message);
        assert_eq!(actors.len(), 1);
        assert_eq!(actors[0].id, "ou_reader_trigger", "换成能认的编号");
        assert_eq!(actors[0].name.as_deref(), Some("测试同事"));
    }
}

/// 契约 hhh-im client v31 §2 第 4 条：一页至多当场补读 5 条；补读失败的卡片照常显示、提及照旧。
#[tokio::test]
async fn card_mention_reads_are_capped_per_page_and_failures_keep_the_card() {
    let (server, client) = setup().await;
    let items: Vec<Value> = (1..=7).map(|i| at_card(&format!("om_a{i}"))).collect();
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages"))
        .and(query_param("card_msg_content_type", "user_card_content"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": 0, "data": {"items": items, "has_more": false}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(query_param("card_msg_content_type", "raw_card_content"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let page = client
        .list_messages(&MessageQuery::recent(chat()))
        .await
        .unwrap();
    assert_eq!(page.items.len(), 7);
    for message in &page.items {
        let actors = card_mention_actors(message);
        assert_eq!(actors.len(), 1);
        assert_eq!(actors[0].id, "ou_card_app_trigger", "没对上：照旧");
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(raw_card_reads(&requests), 5, "每页至多补读 5 条");
}

/// 契约 hhh-im client v31 §2 第 4、6 条：按编号读一条（视野里的重读也走这里）时同样对上。
#[tokio::test]
async fn reading_one_card_matches_its_mentions() {
    let (server, client) = setup().await;
    let reference = MessageRef {
        conversation: chat(),
        id: "om_at".into(),
    };
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages/om_at"))
        .and(query_param("card_msg_content_type", "user_card_content"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": 0, "data": {"items": [at_card("om_at")]}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages/om_at"))
        .and(query_param("card_msg_content_type", "raw_card_content"))
        .respond_with(ResponseTemplate::new(200).set_body_json(at_card_structure("om_at")))
        .expect(1)
        .mount(&server)
        .await;
    let message = client.get_message(&reference).await.unwrap();
    assert_eq!(card_mention_actors(&message)[0].id, "ou_reader_trigger");
}

#[tokio::test]
async fn reading_one_card_asks_for_raw_json_and_falls_back_the_same_way() {
    let (server, client) = setup().await;
    let reference = MessageRef {
        conversation: chat(),
        id: "om_card".into(),
    };
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages/om_card"))
        .and(query_param("card_msg_content_type", "user_card_content"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"code": 0, "data": {"items": [
                card_message("om_card", json!({"schema": "3.0"}))
            ]}}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages/om_card"))
        .and(wiremock::matchers::query_param_is_missing(
            "card_msg_content_type",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"code": 0, "data": {"items": [
                card_message("om_card", json!({"title": "审批", "elements": null}))
            ]}}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let message = client.get_message(&reference).await.unwrap();
    assert!(
        matches!(message.content.as_slice(), [ImContent::Card(card)] if card.title.as_deref() == Some("审批"))
    );
    assert!(message.incomplete);
}

/// 契约 hhh-im client v9：查询卡住最多等 10 秒就放弃（网络失败），发送仍用原来 30 秒的超时。
/// 两个请求都让本地假服务 11 秒后才回应，同时发出；整个测试约 11 秒。
#[tokio::test]
async fn queries_give_up_after_ten_seconds_but_sends_keep_their_longer_timeout() {
    let (server, client) = setup().await;
    let slow = std::time::Duration::from_secs(11);
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":0,"data":{"items":[],"has_more":false}}))
                .set_delay(slow),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/open-apis/im/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":0,"data":raw_message()}))
                .set_delay(slow),
        )
        .mount(&server)
        .await;
    let started = std::time::Instant::now();
    let query = MessageQuery::recent(chat());
    let (listed, sent) = tokio::join!(
        async {
            let result = client.list_messages(&query).await;
            (result, started.elapsed())
        },
        async {
            let result = client.send(&request("slow-send")).await;
            (result, started.elapsed())
        },
    );
    let (listed, listed_after) = listed;
    assert_eq!(
        listed.unwrap_err().kind,
        ErrorKind::Disconnected,
        "查询卡住按网络失败返回"
    );
    assert!(
        listed_after >= std::time::Duration::from_millis(9_500)
            && listed_after < std::time::Duration::from_millis(10_900),
        "查询约 10 秒放弃，实际 {listed_after:?}"
    );
    let (sent, sent_after) = sent;
    assert!(
        matches!(sent.unwrap(), SendOutcome::Accepted { .. }),
        "发送等到了回应"
    );
    assert!(sent_after >= slow, "发送没有被 10 秒截断");
}
