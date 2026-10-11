//! 协议服务的端到端测试：测试当宿主（用协议包里宿主那一侧的连接层），插件在进程内跑，飞书与授权中继都是本机的
//! 假服务（wiremock）。核对握手、开会话、交接、两种登录、续期、读取的请求数、资源分段与上传发送。

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use hhh_im_feishu::FeishuConfig;
use hhh_im_feishu::server::{self, Options};
use hhh_im_protocol::rpc::{RpcError, RpcMessage, codec};
use hhh_im_protocol::transport::{Connection, Incoming, ProviderTransport};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc};
use wiremock::matchers::{body_partial_json, header, method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// 进程内的一条管道，宿主那一头。
struct Pipe {
    write: Mutex<tokio::io::WriteHalf<tokio::io::DuplexStream>>,
    read: Mutex<tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>>,
}

impl ProviderTransport for Pipe {
    fn send(
        &self,
        msg: RpcMessage,
    ) -> Pin<Box<dyn std::future::Future<Output = std::io::Result<()>> + Send + '_>> {
        Box::pin(async move {
            let line = codec::encode_line(&msg).map_err(std::io::Error::other)?;
            let mut write = self.write.lock().await;
            write.write_all(line.as_bytes()).await?;
            write.flush().await
        })
    }

    fn recv(&self) -> Pin<Box<dyn std::future::Future<Output = Option<RpcMessage>> + Send + '_>> {
        Box::pin(async move {
            loop {
                let line = self.read.lock().await.next_line().await.ok()??;
                if line.trim().is_empty() {
                    continue;
                }
                match codec::decode_line(&line) {
                    Ok(message) => return Some(message),
                    Err(error) => panic!("插件写了一行不是 JSON-RPC：{error}：{line}"),
                }
            }
        })
    }
}

/// 测试里的宿主：连接与插件主动发来的消息。
struct Host {
    conn: Arc<Connection>,
    incoming: Mutex<mpsc::UnboundedReceiver<Incoming>>,
}

impl Host {
    async fn start(options: Options) -> Self {
        let (host_side, plugin_side) = tokio::io::duplex(8 << 20);
        let (plugin_read, plugin_write) = tokio::io::split(plugin_side);
        tokio::spawn(server::serve(plugin_read, plugin_write, options));
        let (host_read, host_write) = tokio::io::split(host_side);
        let conn = Connection::new(Arc::new(Pipe {
            write: Mutex::new(host_write),
            read: Mutex::new(BufReader::new(host_read).lines()),
        }));
        let (tx, rx) = mpsc::unbounded_channel();
        let reader = conn.clone();
        tokio::spawn(async move {
            while let Some(incoming) = reader.next_incoming().await {
                // log/write 不看。
                if matches!(&incoming, Incoming::Notification { method, .. } if method == "log/write")
                {
                    continue;
                }
                if tx.send(incoming).is_err() {
                    break;
                }
            }
        });
        Self {
            conn,
            incoming: Mutex::new(rx),
        }
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        tokio::time::timeout(Duration::from_secs(20), self.conn.call(method, params))
            .await
            .unwrap_or_else(|_| panic!("{method} 没有在 20 秒内回复"))
    }

    /// 插件主动发来的下一件事。
    async fn next(&self) -> Incoming {
        tokio::time::timeout(Duration::from_secs(20), self.incoming.lock().await.recv())
            .await
            .expect("插件 20 秒内没再发东西")
            .expect("连接断了")
    }

    /// 下一件必须是 `credential/save`：给出参数，按 `ok` 回复。
    async fn expect_save(&self, ok: bool) -> Value {
        match self.next().await {
            Incoming::Request { id, method, params } if method == "credential/save" => {
                if ok {
                    self.conn.respond(id, json!({})).await.unwrap();
                } else {
                    self.conn
                        .respond_err(
                            id,
                            RpcError::from_kind(
                                hhh_im_protocol::ErrorKind::PermissionDenied,
                                "凭据库写不进",
                            ),
                        )
                        .await
                        .unwrap();
                }
                params
            }
            other => panic!("等的是 credential/save，来的是 {other:?}"),
        }
    }

    /// 下一件必须是某个通知：给出参数。
    async fn expect_notification(&self, name: &str) -> Value {
        match self.next().await {
            Incoming::Notification { method, params } if method == name => params,
            other => panic!("等的是 {name}，来的是 {other:?}"),
        }
    }

    async fn initialize(&self) -> Value {
        self.call(
            "initialize",
            json!({ "protocolVersions": [1], "host": { "name": "HHH", "version": "0.9.0" }, "locale": "zh-CN",
                    "tempDir": std::env::temp_dir(), "network": {} }),
        )
        .await
        .unwrap()
    }
}

fn options(feishu: &MockServer) -> Options {
    Options {
        config: FeishuConfig {
            api_base: feishu.uri(),
            authorize_url: format!("{}/open-apis/authen/v1/authorize", feishu.uri()),
            token_url: format!("{}/oauth/v3/token", feishu.uri()),
        },
        relay_first_poll: Duration::from_millis(20),
    }
}

/// 假飞书的本人身份：只认这个令牌。
async fn mock_identity(feishu: &MockServer, token: &str) {
    Mock::given(method("GET"))
        .and(path("/open-apis/authen/v1/user_info"))
        .and(header("authorization", format!("Bearer {token}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"code":0,"data":{"open_id":"ou_me","name":"Alice","tenant_key":"tenant",
                   "avatar_url":"https://s1-imfile.feishucdn.com/static-resource/v1/v3_me~?image_size=72x72"}}),
        ))
        .mount(feishu)
        .await;
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// 一份插件凭据（自己的应用）。
fn own_credential(access: &str, expires_at: u64) -> String {
    json!({ "version": 1, "accessToken": access, "refreshToken": "ur-old", "expiresAt": expires_at,
            "scopes": ["offline_access"], "login": { "method": "own", "appId": "cli_own", "appSecret": "own-secret" } })
    .to_string()
}

/// 一份插件凭据（通过中继）。
fn relay_credential(access: &str, expires_at: u64, relay: &str) -> String {
    json!({ "version": 1, "accessToken": access, "refreshToken": "ur-old", "expiresAt": expires_at,
            "login": { "method": "relay", "relayUrl": relay } })
    .to_string()
}

fn kind_of(error: &RpcError) -> hhh_im_protocol::ErrorKind {
    error.data.as_ref().unwrap().kind
}

fn user_message(error: &RpcError) -> String {
    error
        .data
        .as_ref()
        .unwrap()
        .user_message
        .clone()
        .unwrap_or_default()
}

/// 握手结果与样例逐项相同（样例拷进 HHH，宿主那边核对平台说明与内置飞书一致）；说不出 v1 的宿主被拒绝。
#[tokio::test]
async fn initialize_declares_the_builtin_feishu_platform() {
    let feishu = MockServer::start().await;
    let host = Host::start(options(&feishu)).await;
    let result = host.initialize().await;
    let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/feishu-protocol/initialize.json");
    if std::env::var_os("HHH_UPDATE_GOLDENS").is_some() {
        std::fs::write(
            &golden,
            serde_json::to_string_pretty(&result).unwrap() + "\n",
        )
        .unwrap();
    }
    let expected: Value = serde_json::from_slice(&std::fs::read(&golden).unwrap()).unwrap();
    assert_eq!(result, expected);
    assert_eq!(result, server::initialize_result());
    let parsed: hhh_im_protocol::method::InitializeResult =
        serde_json::from_value(result.clone()).unwrap();
    parsed
        .capabilities
        .check_consistent(parsed.platform.history)
        .unwrap();
    assert_eq!(
        hhh_im_protocol::method::resolve_callback_path(
            parsed.login_methods[1].callback_path.as_deref().unwrap(),
            "feishu"
        )
        .as_deref(),
        Some("/hhh-friend-im/feishu/callback"),
        "用户已经登记在飞书后台的回调地址"
    );
    let refused = host
        .call(
            "initialize",
            json!({ "protocolVersions": [2], "host": { "name": "HHH", "version": "9" }, "locale": "zh-CN",
                    "tempDir": "", "network": {} }),
        )
        .await
        .unwrap_err();
    assert_eq!(kind_of(&refused), hhh_im_protocol::ErrorKind::Unsupported);
    assert!(host.call("nope/nope", json!({})).await.is_err());
}

/// 存着的凭据开会话：核验出 open_id 的账号；读取的结果带着向飞书发了几个请求。
#[tokio::test]
async fn a_saved_credential_opens_online_and_reads_count_platform_requests() {
    let feishu = MockServer::start().await;
    mock_identity(&feishu, "u-saved").await;
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/chats"))
        .and(header("authorization", "Bearer u-saved"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":{
            "items":[{"chat_id":"oc_team","name":"产品研发群","chat_mode":"group","avatar":"https://s1-imfile.feishucdn.com/static-resource/v1/v3_team~"}],
            "has_more":true,"page_token":"next-page"}})))
        .expect(2)
        .mount(&feishu)
        .await;
    let host = Host::start(options(&feishu)).await;
    host.initialize().await;
    let open = host
        .call(
            "session/open",
            json!({ "credential": own_credential("u-saved", now_secs() + 7200), "settings": {} }),
        )
        .await
        .unwrap();
    assert_eq!(open["state"], "online");
    assert_eq!(open["account"]["id"], "ou_me");
    assert_eq!(open["account"]["idType"], "open_id");
    assert_eq!(open["account"]["name"], "Alice");
    assert!(open["account"].get("tenant").is_none(), "工作区不给");
    let page = host
        .call("conversations/list", json!({ "limit": 20 }))
        .await
        .unwrap();
    assert_eq!(page["platformRequests"], 1);
    assert_eq!(
        page["items"][0],
        json!({"id":"oc_team","kind":"group","name":"产品研发群",
        "avatar":"https://s1-imfile.feishucdn.com/static-resource/v1/v3_team~"})
    );
    let cursor = page["next"].as_str().unwrap().to_owned();
    let second = host
        .call(
            "conversations/list",
            json!({ "limit": 20, "cursor": cursor }),
        )
        .await
        .unwrap();
    assert_eq!(second["platformRequests"], 1);
    let foreign = host
        .call(
            "conversations/list",
            json!({ "limit": 10, "cursor": cursor }),
        )
        .await
        .unwrap_err();
    assert_eq!(
        kind_of(&foreign),
        hhh_im_protocol::ErrorKind::InvalidRequest,
        "游标绑着查询条件"
    );
    let requests = feishu.received_requests().await.unwrap();
    assert!(
        requests.iter().any(|r| r
            .url
            .query_pairs()
            .any(|(k, v)| k == "page_token" && v == "next-page")),
        "游标带回了飞书的分页位置"
    );
}

/// 附录 D：接过内置飞书的授权——先存插件自己的凭据、宿主回了成功，才回在线；带回要写进设置的登录方式。
#[tokio::test]
async fn handover_saves_first_then_answers_online_with_the_login_in_use() {
    let feishu = MockServer::start().await;
    mock_identity(&feishu, "u-builtin").await;
    let host = Arc::new(Host::start(options(&feishu)).await);
    host.initialize().await;
    let legacy = json!({ "format": "hhh-builtin-v1",
        "credential": json!({"accessToken":"u-builtin","refreshToken":"ur-builtin","expiresAt": now_secs() + 7200,
                             "scopes":["offline_access"],"account":{"id":"ou_me","name":"Alice"}}).to_string(),
        "settings": { "mode": "relay", "accountName": "Alice" } });
    let caller = host.clone();
    let open = tokio::spawn(async move {
        caller
            .call("session/open", json!({ "settings": {}, "legacy": legacy }))
            .await
    });
    let saved = match host.next().await {
        Incoming::Request { id, method, params } if method == "credential/save" => {
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert!(!open.is_finished(), "宿主回复存好之前不回在线");
            host.conn.respond(id, json!({})).await.unwrap();
            params
        }
        other => panic!("等的是 credential/save，来的是 {other:?}"),
    };
    let credential: Value = serde_json::from_str(saved["credential"].as_str().unwrap()).unwrap();
    assert_eq!(credential["accessToken"], "u-builtin");
    assert_eq!(
        credential["login"],
        json!({ "method": "relay" }),
        "默认中继不写地址"
    );
    assert_eq!(credential["account"]["id"], "ou_me");
    assert!(saved["renewAtMs"].as_i64().unwrap() > 0);
    let open = open.await.unwrap().unwrap();
    assert_eq!(open["state"], "online");
    assert_eq!(
        open["account"]["id"], "ou_me",
        "账号与内置飞书的 open_id 相同"
    );
    assert_eq!(
        open["adoptedSettings"],
        json!({ "login": { "method": "relay", "values": { "relayUrl": "https://hhh-im-auth.magiclizi.com" } } })
    );

    // 自己的应用：App ID 与明文的 Secret 一起交过来，交接后登录框里照样是“已保存”。
    let host = Host::start(options(&feishu)).await;
    host.initialize().await;
    let legacy = json!({ "format": "hhh-builtin-v1",
        "credential": json!({"accessToken":"u-builtin","expiresAt": now_secs() + 7200}).to_string(),
        "settings": { "appId": "cli_own", "appSecret": "own-secret" } });
    let call = host.call("session/open", json!({ "settings": {}, "legacy": legacy }));
    let (open, saved) = tokio::join!(call, host.expect_save(true));
    let credential: Value = serde_json::from_str(saved["credential"].as_str().unwrap()).unwrap();
    assert_eq!(credential["login"]["appSecret"], "own-secret");
    assert!(saved.get("renewAtMs").is_none(), "没有刷新令牌就不排续期");
    assert_eq!(
        open.unwrap()["adoptedSettings"]["login"],
        json!({ "method": "own", "values": { "appId": "cli_own", "appSecret": "own-secret" } })
    );

    // 内置那份用不了：回需要登录，不存。
    let host = Host::start(options(&feishu)).await;
    host.initialize().await;
    let legacy = json!({ "format": "hhh-builtin-v1",
        "credential": json!({"accessToken":"u-builtin"}).to_string(), "settings": { "appId": "cli_own" } });
    let open = host
        .call("session/open", json!({ "settings": {}, "legacy": legacy }))
        .await
        .unwrap();
    assert_eq!(open["state"], "needsLogin");
    assert_eq!(open["message"], "飞书应用配置不完整，请在设置里重新登录。");
}

/// 中继登记时回 state：照请求里的 state 回（中继的协议）。
struct Registered {
    base: String,
}

impl Respond for Registered {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["platform"], "feishu");
        assert_eq!(body["exchange"], true);
        ResponseTemplate::new(201).set_body_json(json!({
            "state": body["state"], "claimToken": "claim-secret",
            "callbackUrl": format!("{}/hhh-friend-im/feishu/callback", self.base),
            "expiresIn": 300, "clientId": "cli_relay"
        }))
    }
}

/// 领取：照路径里的 state 回令牌。
struct Claimed;

impl Respond for Claimed {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let state = request.url.path().split('/').nth(3).unwrap().to_owned();
        ResponseTemplate::new(200).set_body_json(json!({
            "status": "completed", "state": state,
            "result": { "type": "token", "token": { "access_token": "u-relay", "refresh_token": "ur-relay",
                        "expires_in": 7200, "token_type": "Bearer", "scope": "offline_access im:message" } }
        }))
    }
}

/// 通过中继登录：先登记（中继的 App ID 拼授权页）、推“打开授权页”一步，领取到令牌后先存、宿主回了成功才算登录完成。
#[tokio::test]
async fn relay_login_registers_polls_saves_and_then_finishes() {
    let feishu = MockServer::start().await;
    mock_identity(&feishu, "u-relay").await;
    let relay = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/flows"))
        .respond_with(Registered { base: relay.uri() })
        .expect(1)
        .mount(&relay)
        .await;
    Mock::given(method("POST"))
        .and(path_regex("^/v1/flows/[^/]+/claim$"))
        .and(header("authorization", "Bearer claim-secret"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"status":"pending","retryAfterMs":500})),
        )
        .up_to_n_times(1)
        .mount(&relay)
        .await;
    Mock::given(method("POST"))
        .and(path_regex("^/v1/flows/[^/]+/claim$"))
        .respond_with(Claimed)
        .mount(&relay)
        .await;
    let host = Host::start(options(&feishu)).await;
    host.initialize().await;
    let open = host
        .call("session/open", json!({ "settings": {} }))
        .await
        .unwrap();
    assert_eq!(open["reason"], "noCredential");

    // 中继地址不对：就地说原因，不开始。
    let refused = host
        .call(
            "login/start",
            json!({ "method": "relay", "values": { "relayUrl": "http://relay.example" } }),
        )
        .await
        .unwrap_err();
    assert_eq!(
        user_message(&refused),
        "中继地址要用 https（本机测试可以用 http://127.0.0.1）。"
    );

    let started = host
        .call(
            "login/start",
            json!({ "method": "relay", "values": { "relayUrl": relay.uri() } }),
        )
        .await
        .unwrap();
    let flow = started["flowId"].as_str().unwrap().to_owned();
    let step = host.expect_notification("login/step").await;
    assert_eq!(step["flowId"], flow);
    assert_eq!(step["step"]["kind"], "openUrl");
    let url = reqwest::Url::parse(step["step"]["url"].as_str().unwrap()).unwrap();
    let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(pairs["client_id"], "cli_relay", "授权页用中继的 App ID");
    assert_eq!(
        pairs["redirect_uri"],
        format!("{}/hhh-friend-im/feishu/callback", relay.uri())
    );
    assert!(pairs["scope"].contains("im:message.reactions:read"));
    let saved = host.expect_save(true).await;
    let credential: Value = serde_json::from_str(saved["credential"].as_str().unwrap()).unwrap();
    assert_eq!(
        credential["login"],
        json!({ "method": "relay", "relayUrl": relay.uri() })
    );
    assert_eq!(credential["refreshToken"], "ur-relay");
    let renew_at = saved["renewAtMs"].as_i64().unwrap();
    let expected = (now_secs() as i64 + 7200 - 300) * 1000;
    assert!(
        (renew_at - expected).abs() < 10_000,
        "过期前 5 分钟续期：{renew_at} vs {expected}"
    );
    let finished = host.expect_notification("login/finished").await;
    assert_eq!(finished["flowId"], flow);
    assert_eq!(finished["account"]["id"], "ou_me");
    assert_eq!(finished["account"]["idType"], "open_id");
    // 登录完成就在线了。
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/chats"))
        .and(header("authorization", "Bearer u-relay"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":0,"data":{"items":[],"has_more":false}})),
        )
        .mount(&feishu)
        .await;
    host.call("conversations/list", json!({ "limit": 5 }))
        .await
        .unwrap();
}

/// 中继没配飞书应用：同 Friend 的说法，不打开授权页。
#[tokio::test]
async fn a_relay_without_a_feishu_app_says_so() {
    let feishu = MockServer::start().await;
    let relay = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/flows"))
        .respond_with(
            ResponseTemplate::new(409).set_body_json(
                json!({"error":{"code":"feishu_exchange_unavailable","message":"x"}}),
            ),
        )
        .mount(&relay)
        .await;
    let host = Host::start(options(&feishu)).await;
    host.initialize().await;
    let refused = host
        .call(
            "login/start",
            json!({ "method": "relay", "values": { "relayUrl": relay.uri() } }),
        )
        .await
        .unwrap_err();
    assert_eq!(
        user_message(&refused),
        "这个中继没有配置飞书应用，可以改用自己的应用。"
    );
}

/// 使用自己的应用：授权页回到宿主给的本机地址，宿主经 login/callback 交回来，在本机换令牌、先存再完成。
#[tokio::test]
async fn own_app_login_waits_for_the_callback_and_exchanges_locally() {
    let feishu = MockServer::start().await;
    mock_identity(&feishu, "u-own").await;
    Mock::given(method("POST"))
        .and(path("/oauth/v3/token"))
        .and(body_partial_json(json!({"grant_type":"authorization_code","client_id":"cli_own",
            "client_secret":"own-secret","code":"the-code",
            "redirect_uri":"http://localhost:29527/hhh-friend-im/feishu/callback"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code":0,"access_token":"u-own",
            "refresh_token":"ur-own","expires_in":7200,"token_type":"Bearer","scope":"offline_access"})))
        .expect(1)
        .mount(&feishu)
        .await;
    let host = Host::start(options(&feishu)).await;
    host.initialize().await;
    host.call("session/open", json!({ "settings": {} }))
        .await
        .unwrap();
    let missing = host
        .call("login/start", json!({ "method": "own", "values": { "appId": "cli_own" },
            "callbackUrl": "http://localhost:29527/hhh-friend-im/feishu/callback", "state": "s".repeat(48) }))
        .await
        .unwrap_err();
    assert_eq!(
        user_message(&missing),
        "首次使用自己的应用需要填写 App Secret。"
    );
    let state = "s".repeat(48);
    let started = host
        .call("login/start", json!({ "method": "own", "values": { "appId": "cli_own", "appSecret": "own-secret" },
            "callbackUrl": "http://localhost:29527/hhh-friend-im/feishu/callback", "state": state }))
        .await
        .unwrap();
    let flow = started["flowId"].as_str().unwrap().to_owned();
    let step = host.expect_notification("login/step").await;
    let url = reqwest::Url::parse(step["step"]["url"].as_str().unwrap()).unwrap();
    let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(pairs["state"], state, "用宿主给的一次性 state");
    assert_eq!(pairs["client_id"], "cli_own");
    let stale = host
        .call(
            "login/callback",
            json!({ "flowId": "another", "url": "http://localhost:29527/x" }),
        )
        .await
        .unwrap_err();
    assert_eq!(
        user_message(&stale),
        "没有等待中的飞书登录，或这次登录已经结束。"
    );
    host.call(
        "login/callback",
        json!({ "flowId": flow, "url": format!("http://localhost:29527/hhh-friend-im/feishu/callback?state={state}&code=the-code") }),
    )
    .await
    .unwrap();
    let saved = host.expect_save(true).await;
    let credential: Value = serde_json::from_str(saved["credential"].as_str().unwrap()).unwrap();
    assert_eq!(
        credential["login"],
        json!({ "method": "own", "appId": "cli_own", "appSecret": "own-secret" })
    );
    let finished = host.expect_notification("login/finished").await;
    assert_eq!(finished["account"]["id"], "ou_me");

    // 再登录一次不填 Secret：同一个 App ID 沿用凭据里的；换了 App ID 要填。
    let again = host
        .call("login/start", json!({ "method": "own", "values": { "appId": "cli_own" },
            "callbackUrl": "http://localhost:29527/hhh-friend-im/feishu/callback", "state": "t".repeat(48) }))
        .await
        .unwrap();
    host.expect_notification("login/step").await;
    let changed = host
        .call("login/start", json!({ "method": "own", "values": { "appId": "cli_other" },
            "callbackUrl": "http://localhost:29527/hhh-friend-im/feishu/callback", "state": "u".repeat(48) }))
        .await
        .unwrap_err();
    assert_eq!(
        user_message(&changed),
        "换了 App ID 需要同时填写它的 App Secret。"
    );
    // 取消等待：报“已取消”。
    host.conn
        .notify("login/cancel", json!({ "flowId": again["flowId"] }))
        .await
        .unwrap();
    let failed = host.expect_notification("login/failed").await;
    assert_eq!(failed["message"], "已取消飞书登录。");
}

/// 宿主叫续期（自己的应用）：本机换新、先存（带下次续期的时间）再回成功，之后用新令牌。
#[tokio::test]
async fn renewal_refreshes_saves_and_switches_to_the_new_token() {
    let feishu = MockServer::start().await;
    mock_identity(&feishu, "u-old").await;
    mock_identity(&feishu, "u-new").await;
    Mock::given(method("POST"))
        .and(path("/oauth/v3/token"))
        .and(body_partial_json(
            json!({"grant_type":"refresh_token","refresh_token":"ur-old",
            "client_id":"cli_own","client_secret":"own-secret"}),
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code":0,"access_token":"u-new",
            "refresh_token":"ur-new","expires_in":7200,"token_type":"Bearer"})),
        )
        .expect(1)
        .mount(&feishu)
        .await;
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/chats"))
        .and(header("authorization", "Bearer u-new"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":0,"data":{"items":[],"has_more":false}})),
        )
        .expect(1)
        .mount(&feishu)
        .await;
    let host = Host::start(options(&feishu)).await;
    host.initialize().await;
    let credential = own_credential("u-old", now_secs() + 7200);
    host.call(
        "session/open",
        json!({ "credential": credential, "settings": {} }),
    )
    .await
    .unwrap();
    let call = host.call("session/renew", json!({ "credential": credential }));
    let (renewed, saved) = tokio::join!(call, host.expect_save(true));
    assert_eq!(renewed.unwrap(), json!({}));
    let fresh: Value = serde_json::from_str(saved["credential"].as_str().unwrap()).unwrap();
    assert_eq!(
        (
            fresh["accessToken"].as_str(),
            fresh["refreshToken"].as_str()
        ),
        (Some("u-new"), Some("ur-new"))
    );
    assert_eq!(fresh["login"]["appId"], "cli_own", "登录方式照旧");
    assert!(saved["renewAtMs"].as_i64().unwrap() > (now_secs() as i64 + 6000) * 1000);
    host.call("conversations/list", json!({ "limit": 5 }))
        .await
        .unwrap();
}

/// 快到期的凭据：开会话时先续期（存好再用），不等用户读时才发现。
#[tokio::test]
async fn an_expiring_credential_is_renewed_when_the_session_opens() {
    let feishu = MockServer::start().await;
    mock_identity(&feishu, "u-new").await;
    Mock::given(method("POST"))
        .and(path("/oauth/v3/token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code":0,"access_token":"u-new",
            "refresh_token":"ur-new","expires_in":7200,"token_type":"Bearer"})),
        )
        .expect(1)
        .mount(&feishu)
        .await;
    let host = Host::start(options(&feishu)).await;
    host.initialize().await;
    let call = host.call(
        "session/open",
        json!({ "credential": own_credential("u-old", now_secs() + 60), "settings": {} }),
    );
    let (open, saved) = tokio::join!(call, host.expect_save(true));
    assert_eq!(open.unwrap()["state"], "online");
    assert!(saved["credential"].as_str().unwrap().contains("u-new"));
}

/// 经中继续期：没在线时也能续（宿主临时起插件、不开账号）；飞书拒绝就是登录失效（同 Friend 的说法），在线时另发
/// session/expired，之后的请求一律回授权失效。
#[tokio::test]
async fn relay_renewal_works_offline_and_a_rejection_expires_the_login() {
    let feishu = MockServer::start().await;
    mock_identity(&feishu, "u-old").await;
    let relay = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/feishu/refresh"))
        .and(body_partial_json(json!({"refreshToken":"ur-old"})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"token":{"access_token":"u-new",
            "refresh_token":"ur-new","expires_in":7200,"token_type":"Bearer"}})),
        )
        .up_to_n_times(1)
        .mount(&relay)
        .await;
    let host = Host::start(options(&feishu)).await;
    host.initialize().await;
    let credential = relay_credential("u-old", now_secs() + 7200, &relay.uri());
    let call = host.call("session/renew", json!({ "credential": credential }));
    let (renewed, saved) = tokio::join!(call, host.expect_save(true));
    assert_eq!(renewed.unwrap(), json!({}));
    assert!(saved["credential"].as_str().unwrap().contains("ur-new"));

    Mock::given(method("POST"))
        .and(path("/v1/feishu/refresh"))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            json!({"error":{"code":"feishu_refresh_rejected","feishuCode":20026,"message":"x"}}),
        ))
        .mount(&relay)
        .await;
    let host = Host::start(options(&feishu)).await;
    host.initialize().await;
    host.call(
        "session/open",
        json!({ "credential": credential, "settings": {} }),
    )
    .await
    .unwrap();
    let refused = host
        .call("session/renew", json!({ "credential": credential }))
        .await
        .unwrap_err();
    assert_eq!(kind_of(&refused), hhh_im_protocol::ErrorKind::AuthExpired);
    // 错误里是 Friend 的话，通知里是重新登录卡片上的话（同内置）。
    assert_eq!(user_message(&refused), "飞书登录已失效，请重新登录。");
    let expired = host.expect_notification("session/expired").await;
    assert_eq!(
        expired,
        json!({ "reason": "expired", "message": "重新登录后，就能继续选择群聊或好友。" })
    );
    let after = host
        .call("conversations/list", json!({ "limit": 5 }))
        .await
        .unwrap_err();
    assert_eq!(kind_of(&after), hhh_im_protocol::ErrorKind::AuthExpired);
}

/// 飞书在使用中说授权无效：强制续期一次、再做一次；续上了照常回结果。
#[tokio::test]
async fn a_rejected_token_is_renewed_once_and_the_read_retried() {
    let feishu = MockServer::start().await;
    mock_identity(&feishu, "u-old").await;
    mock_identity(&feishu, "u-new").await;
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/chats"))
        .and(header("authorization", "Bearer u-old"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":99991668,"msg":"invalid token"})),
        )
        .mount(&feishu)
        .await;
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/chats"))
        .and(header("authorization", "Bearer u-new"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":0,"data":{"items":[],"has_more":false}})),
        )
        .mount(&feishu)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/v3/token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code":0,"access_token":"u-new",
            "refresh_token":"ur-new","expires_in":7200,"token_type":"Bearer"})),
        )
        .expect(1)
        .mount(&feishu)
        .await;
    let host = Host::start(options(&feishu)).await;
    host.initialize().await;
    host.call(
        "session/open",
        json!({ "credential": own_credential("u-old", now_secs() + 7200), "settings": {} }),
    )
    .await
    .unwrap();
    let call = host.call("conversations/list", json!({ "limit": 5 }));
    let (page, saved) = tokio::join!(call, host.expect_save(true));
    let page = page.unwrap();
    assert_eq!(page["items"], json!([]));
    assert_eq!(page["platformRequests"], 2, "被拒的那次与重试的那次都算");
    assert!(saved["credential"].as_str().unwrap().contains("u-new"));
}

/// 借来的凭据：不续期、不存。打开时已经过期，回“暂时连不上”并说下一步（同 Friend 借用的授权：
/// `GrantError::Unavailable`）；用着用着被飞书拒绝，发 `session/expired` 说下一步（宿主据此重读借出方的凭据、变了就重开），
/// 这次与之后的请求都回暂时连不上。
#[tokio::test]
async fn borrowed_credentials_are_never_renewed() {
    const BORROWED: &str =
        "隔离测试不刷新你的飞书授权：请打开你的 Friend 刷新，或在测试里重新授权。";
    let feishu = MockServer::start().await;
    let host = Host::start(options(&feishu)).await;
    host.initialize().await;
    let refused = host
        .call(
            "session/open",
            json!({ "credential": own_credential("u-old", now_secs() - 10), "settings": {}, "credentialReadOnly": true }),
        )
        .await
        .unwrap_err();
    assert_eq!(kind_of(&refused), hhh_im_protocol::ErrorKind::Disconnected);
    assert_eq!(user_message(&refused), BORROWED);
    assert!(
        feishu.received_requests().await.unwrap().is_empty(),
        "没有去续期"
    );

    // 还没过期的借来的凭据照用；飞书拒绝它时不强制续期，同样回暂时连不上。
    mock_identity(&feishu, "u-old").await;
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/chats"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":99991668,"msg":"invalid token"})),
        )
        .mount(&feishu)
        .await;
    let host = Host::start(options(&feishu)).await;
    host.initialize().await;
    let open = host
        .call(
            "session/open",
            json!({ "credential": own_credential("u-old", now_secs() + 7200), "settings": {}, "credentialReadOnly": true }),
        )
        .await
        .unwrap();
    assert_eq!(open["state"], "online");
    let refused = host
        .call("conversations/list", json!({ "limit": 5 }))
        .await
        .unwrap_err();
    assert_eq!(kind_of(&refused), hhh_im_protocol::ErrorKind::Disconnected);
    assert_eq!(user_message(&refused), BORROWED);
    let expired = host.expect_notification("session/expired").await;
    assert_eq!(expired, json!({ "reason": "expired", "message": BORROWED }));
    let after = host
        .call("conversations/list", json!({ "limit": 5 }))
        .await
        .unwrap_err();
    assert_eq!(kind_of(&after), hhh_im_protocol::ErrorKind::Disconnected);
    assert_eq!(user_message(&after), BORROWED);
    let requests = feishu.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .all(|request| request.url.path() != "/oauth/v3/token"),
        "没有去续期"
    );
}

/// 读文件：宿主按 1 MiB 一段来要，插件只开一条读流接着读；`total` 与内容指纹尽早给，最后一段标 `eof`。
#[tokio::test]
async fn sequential_resource_reads_share_one_stream() {
    use base64::Engine as _;
    let feishu = MockServer::start().await;
    mock_identity(&feishu, "u-saved").await;
    let body: Vec<u8> = (0..(2 * 1024 * 1024 + 4096))
        .map(|i| (i % 251) as u8)
        .collect();
    let sha: String = {
        use sha2::Digest as _;
        sha2::Sha256::digest(&body)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages/om_file/resources/file_1"))
        .and(query_param("type", "file"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "Content-Disposition",
                    format!("attachment; filename=\"{sha}.bin\"").as_str(),
                )
                .set_body_raw(body.clone(), "application/octet-stream"),
        )
        .expect(1)
        .mount(&feishu)
        .await;
    let host = Host::start(options(&feishu)).await;
    host.initialize().await;
    host.call(
        "session/open",
        json!({ "credential": own_credential("u-saved", now_secs() + 7200), "settings": {} }),
    )
    .await
    .unwrap();
    let mut got = Vec::new();
    loop {
        let chunk = host
            .call(
                "resources/read",
                json!({ "conversationId": "oc_group", "messageId": "om_file", "key": "file_1", "kind": "file",
                        "offset": got.len(), "length": 1024 * 1024 }),
            )
            .await
            .unwrap();
        assert_eq!(chunk["total"], body.len());
        assert_eq!(chunk["sha256"], sha);
        got.extend(
            base64::engine::general_purpose::STANDARD
                .decode(chunk["data"].as_str().unwrap())
                .unwrap(),
        );
        if chunk["eof"] == true {
            break;
        }
    }
    assert_eq!(got, body);
}

/// 上传与发送：攒齐、核对摘要后交给飞书拿到资源编号，再带着它发；同一个操作编号再来时回同一个结果、不再发。
#[tokio::test]
async fn uploads_then_sends_once_per_operation() {
    use base64::Engine as _;
    let feishu = MockServer::start().await;
    mock_identity(&feishu, "u-saved").await;
    Mock::given(method("POST"))
        .and(path("/open-apis/im/v1/images"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":0,"data":{"image_key":"img_v3_up"}})),
        )
        .expect(1)
        .mount(&feishu)
        .await;
    let sent: Value =
        serde_json::from_str(include_str!("fixtures/feishu/response_text.json")).unwrap();
    Mock::given(method("POST"))
        .and(path("/open-apis/im/v1/messages"))
        .and(query_param("receive_id_type", "chat_id"))
        .and(body_partial_json(
            json!({"receive_id":"oc_group","msg_type":"image","uuid":"op-1"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code":0,"data":sent})))
        .expect(1)
        .mount(&feishu)
        .await;
    let host = Host::start(options(&feishu)).await;
    host.initialize().await;
    let credential =
        json!({ "version": 1, "accessToken": "u-saved", "expiresAt": now_secs() + 7200,
        "scopes": ["im:resource", "im:message", "im:message.send_as_user"],
        "login": { "method": "own", "appId": "cli_own", "appSecret": "own-secret" } })
        .to_string();
    host.call(
        "session/open",
        json!({ "credential": credential, "settings": {} }),
    )
    .await
    .unwrap();
    let bytes = vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4];
    let too_big = host
        .call(
            "uploads/begin",
            json!({ "kind": "image", "name": "截图.png", "bytes": 11 * 1024 * 1024 }),
        )
        .await
        .unwrap_err();
    assert_eq!(kind_of(&too_big), hhh_im_protocol::ErrorKind::LimitExceeded);
    let begin = host
        .call(
            "uploads/begin",
            json!({ "kind": "image", "name": "截图.png", "bytes": bytes.len() }),
        )
        .await
        .unwrap();
    let upload = begin["uploadId"].as_str().unwrap();
    host.call(
        "uploads/chunk",
        json!({ "uploadId": upload, "offset": 0, "data": base64::engine::general_purpose::STANDARD.encode(&bytes) }),
    )
    .await
    .unwrap();
    let sha: String = {
        use sha2::Digest as _;
        sha2::Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    let finish = host
        .call(
            "uploads/finish",
            json!({ "uploadId": upload, "sha256": sha }),
        )
        .await
        .unwrap();
    assert_eq!(finish["key"], "img_v3_up");
    let request = json!({ "operationId": "op-1", "target": { "type": "conversation", "conversationId": "oc_group" },
        "content": [{ "type": "image", "key": "img_v3_up" }] });
    let first = host.call("messages/send", request.clone()).await.unwrap();
    assert_eq!(first["state"], "accepted");
    assert_eq!(first["message"]["sender"]["idType"], "open_id");
    let again = host.call("messages/send", request).await.unwrap();
    assert_eq!(again, first, "同一个操作编号回同一个结果");
}

/// 读消息：宿主给的毫秒换回飞书的秒；结果是线上消息（与样例同一套翻译）。
#[tokio::test]
async fn message_reads_convert_milliseconds_and_return_wire_messages() {
    let feishu = MockServer::start().await;
    mock_identity(&feishu, "u-saved").await;
    let item: Value =
        serde_json::from_str(include_str!("fixtures/feishu/response_text.json")).unwrap();
    Mock::given(method("GET"))
        .and(path("/open-apis/im/v1/messages"))
        .and(query_param("start_time", "1700000000"))
        .and(query_param("end_time", "1700000100"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":0,"data":{"items":[item],"has_more":false}})),
        )
        .expect(1)
        .mount(&feishu)
        .await;
    let host = Host::start(options(&feishu)).await;
    host.initialize().await;
    host.call(
        "session/open",
        json!({ "credential": own_credential("u-saved", now_secs() + 7200), "settings": {} }),
    )
    .await
    .unwrap();
    let page = host
        .call(
            "messages/list",
            json!({ "conversationId": "oc_group", "afterMs": 1_700_000_000_000i64, "beforeMs": 1_700_000_100_000i64,
                    "newestFirst": false, "limit": 20 }),
        )
        .await
        .unwrap();
    assert_eq!(page["platformRequests"], 1);
    let golden: Value =
        serde_json::from_str(include_str!("fixtures/feishu-protocol/response_text.json")).unwrap();
    // 样例里的消息是按 `test` 会话取样的；这里的假飞书原样回了同一份，线上消息不带连接编号，应完全相同。
    assert_eq!(page["items"][0], golden);
}

/// 续期被拒的三种说法同 HHH 内置飞书：没开刷新授权（20074）、App Secret 失效（20002）、其余是登录失效；
/// 暂时连不上飞书不算失效。错误里写 Friend 的 `ReauthReason::message`，`session/expired` 写重新登录卡片上的话
/// （`connection.rs` 的 `reauth_text`）。
#[tokio::test]
async fn renewal_rejections_use_the_builtin_wordings() {
    const REFRESH_DISABLED: &str = "飞书应用没有开启刷新授权。请在开发者后台「安全设置」开启刷新 user_access_token 并发布新版本，再重新登录。";
    const SECRET_INVALID: &str =
        "飞书 App Secret 已失效，请在设置里重新登录并填写新的 App Secret。";
    for (code, status, reason, error, card) in [
        (
            20074,
            200,
            Some("expired"),
            REFRESH_DISABLED,
            REFRESH_DISABLED,
        ),
        (
            20002,
            200,
            Some("credentialInvalid"),
            SECRET_INVALID,
            SECRET_INVALID,
        ),
        (
            20026,
            200,
            Some("expired"),
            "飞书登录已失效，请重新登录。",
            "重新登录后，就能继续选择群聊或好友。",
        ),
        (-1, 503, None, "", ""),
    ] {
        let feishu = MockServer::start().await;
        mock_identity(&feishu, "u-old").await;
        Mock::given(method("POST"))
            .and(path("/oauth/v3/token"))
            .respond_with(
                ResponseTemplate::new(status).set_body_json(json!({"code": code, "msg": "x"})),
            )
            .mount(&feishu)
            .await;
        let host = Host::start(options(&feishu)).await;
        host.initialize().await;
        let credential = own_credential("u-old", now_secs() + 7200);
        host.call(
            "session/open",
            json!({ "credential": credential, "settings": {} }),
        )
        .await
        .unwrap();
        let refused = host
            .call("session/renew", json!({ "credential": credential }))
            .await
            .unwrap_err();
        match reason {
            Some(reason) => {
                assert_eq!(
                    kind_of(&refused),
                    hhh_im_protocol::ErrorKind::AuthExpired,
                    "{code}"
                );
                assert_eq!(user_message(&refused), error, "{code}");
                let expired = host.expect_notification("session/expired").await;
                assert_eq!(
                    expired,
                    json!({ "reason": reason, "message": card }),
                    "{code}"
                );
                // 之后的请求也回授权失效，话还是 Friend 的。
                let after = host
                    .call("conversations/list", json!({ "limit": 5 }))
                    .await
                    .unwrap_err();
                assert_eq!(user_message(&after), error, "{code}");
            }
            None => {
                assert_eq!(
                    kind_of(&refused),
                    hhh_im_protocol::ErrorKind::Disconnected,
                    "暂时故障"
                );
                // 还在线：照常读（不发 session/expired）。
                Mock::given(method("GET"))
                    .and(path("/open-apis/im/v1/chats"))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_json(json!({"code":0,"data":{"items":[],"has_more":false}})),
                    )
                    .mount(&feishu)
                    .await;
                host.call("conversations/list", json!({ "limit": 5 }))
                    .await
                    .unwrap();
            }
        }
    }
}

/// 打开时登录已经失效（过期且没有刷新令牌）：回需要登录，话写重新登录卡片上的那句（宿主画在卡片上，同内置）。
#[tokio::test]
async fn opening_a_dead_login_shows_the_card_text() {
    let feishu = MockServer::start().await;
    let host = Host::start(options(&feishu)).await;
    host.initialize().await;
    let credential = json!({ "version": 1, "accessToken": "u-old", "expiresAt": now_secs() - 10,
        "login": { "method": "own", "appId": "cli_own", "appSecret": "own-secret" } })
    .to_string();
    let open = host
        .call(
            "session/open",
            json!({ "credential": credential, "settings": {} }),
        )
        .await
        .unwrap();
    assert_eq!(
        open,
        json!({ "state": "needsLogin", "reason": "expired", "message": "重新登录后，就能继续选择群聊或好友。" })
    );
}
