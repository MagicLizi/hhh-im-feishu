# 飞书插件的规矩（给 agent）

这个仓库是 HHH 的飞书 IM 插件，从 HHH 内置的飞书搬出来，走 HHH-IM-Protocol v1。不管用哪个 agent 打开，都按这里的规矩做；
`CLAUDE.md` 只是指到这里。

## 先读

1. 协议：[hhh-im-sdk/docs/protocol-v1.md](https://github.com/MagicLizi/hhh-im-sdk/blob/main/docs/protocol-v1.md)。每个方法的参数和结果都以它为准，
   Rust 类型在 `hhh-im-protocol` 的 `types`、`method`、`platform` 三个模块里。
2. 发版本规范：[hhh-im-sdk/docs/release.md](https://github.com/MagicLizi/hhh-im-sdk/blob/main/docs/release.md)。
3. 本仓库的结构：

   | 位置 | 内容 |
   |---|---|
   | `src/im/`、`src/net.rs`、`src/feishu/` | 从 HHH 搬来的飞书适配器：统一类型、统一 TLS 握手、飞书接口与翻译。平台事实（叫法、限速、登录面板的文字）在 `src/feishu/facts.rs` |
   | `src/server/` | 协议服务。`mod.rs` 负责请求循环，`declare.rs` 写握手声明，`session.rs` 管会话与续期，`login.rs` 和 `relay.rs` 管登录，`methods.rs` 实现读取与动作，`resources.rs` 管资源与上传，`wire.rs` 把统一类型翻成线上类型 |
   | `src/server/wire/host_inverse.rs` | 测试用：HHH 宿主把线上类型翻回来的那一半，照抄自 HHH 的 `hhh-im/src/provider/translate.rs` |
   | `tests/fixtures/feishu/` | 飞书响应的取样，加上内置飞书翻出来的样子（`*.expected.json`） |
   | `tests/fixtures/feishu-protocol/` | 协议样例：插件发给 HHH 的线上消息和握手结果，会拷进 HHH 再核对一遍 |

## 第一原则：体验和内置飞书一样

用户从内置飞书换成这个插件时，界面、说明文字、登录、续期和本机记录都不能有区别：

- 线上消息必须是 HHH 宿主翻译的严格反向：统一类型翻成线上，宿主翻回来，结果要一字不差。`wire/goldens.rs` 逐份核对这一点；
  宿主改了翻译，就同步更新 `host_inverse.rs`。
- 握手声明（`declare.rs`）和登录面板的文字只从 `facts.rs` 取，不另写一份。
- 编号种类（`idType`）、头像键（内置时存下的完整地址）都照内置时的写法，本机的旧记录才认得出是同一个人、同一个头像。
- 续期和失效的说法和 Friend 内置时一样，分两处写（`session.rs` 的 `error_text`、`card_text`）：
  - 授权失效的错误写 Friend 的 `ReauthReason::message`；
  - `session/expired` 和 `session/open` 的需要登录写重新登录卡片上的那几句（`facts::reauth`）。
- 已知的小差别（用户看不出）：消息里 @机器人 时，内置飞书照飞书原样把身份记成 `bot`，插件标 `kind: bot`、宿主记成应用；两边显示的名字一样。

做不到一样时，不要在插件里绕路或伪造协议没有的东西。到 [hhh-im-sdk](https://github.com/MagicLizi/hhh-im-sdk/issues) 提 issue，
写清楚要什么、为什么所有 IM 都可能用得上。HHH 的 IM 层不认识任何一家 IM，不要求它给飞书开特例。

## 插件只做三件事

- **和飞书说话**：登录、读消息、发消息、读资源。
- **翻成统一格式**：飞书的消息、会话、人员、表情都翻成协议的类型，飞书私有的写法不能原样漏给 HHH。
- **如实声明**：`initialize` 里声明平台能做什么，做不到的不声明。

聊天记录、凭据和配置都交给 HHH，插件自己不存：

- 凭据通过 `credential/save` 交给 HHH，等 HHH 回复成功后才用（飞书的刷新令牌只能用一次，必须先存后用），下次从 `session/open` 拿回来。
- 临时文件只放在 `initialize` 给的 `tempDir` 里。
- 标准输出只写协议消息，日志走 `log/write`（`src/server/log.rs`）。
- `src/main.rs` 开头的 `windows_subsystem` 不要删：HHH 拉起插件时不弹控制台窗口，标准输入输出照样能用。

## 夹具与样例

- **夹具里不放真实数据**：人名、群名、消息、图片和文件的编号、日志编号、公司内容一律用编的。新取样先换成编的，输入和期望输出一起换，再提交。
- 改了翻译、样例要跟着变时，用 `HHH_UPDATE_GOLDENS=1 cargo test` 重写 `tests/fixtures/feishu-protocol/`，再逐个看差异，确认每一处变化都是有意的。
- 样例有变化时，在交付说明里列出要拷进 HHH 的文件。

## 改完必须

1. `cargo build --release`、`cargo test`、`cargo fmt --check`、`cargo clippy --all-targets` 全部通过（零警告）。
2. 打包后自检：`powershell -File scripts/pack.ps1`，再跑 `hhh-im-check dist/stage/hhh-im-provider.json`，全部通过。
   自检工具没装时：`cargo install --git https://github.com/MagicLizi/hhh-im-sdk --tag v0.16.0 hhh-im-protocol --features transport,package`。
3. 在 HHH 里试一遍：设置 → IM → 添加 IM → 文件夹，选 `dist/stage`。登录、读、发、续期都走一遍。

## 清单 `hhh-im-provider.json`

`id` 是 `feishu`，**不能改**：HHH 靠它认出这个插件和内置飞书的旧记录，而且它是保留编号，只认官方签名。`imType` 也是 `feishu`。
`version` 和 `Cargo.toml` 的版本保持一致，`tests/manifest.rs` 会核对。目前 `os` 只写 `windows`，只发 `windows-x64` 的包。

## 发版本

用「发版本」skill：步骤正本是私有中控仓库 `MagicLizi/hhh-release` 的 `RELEASING.md`，agent 拟好版本号和更新说明、和用户确认后一次发完。发布只在 hhh-release 里做；要出哪些系统写在 `.github/hhh-plugin.json`。

- `.github/workflows/hhh-plugin.yml`、`.github/hhh-ci/` 与发版 skill 由 hhh-release 统一维护、同步过来，不在这里改。
- 官方私钥只在 hhh-release 的 Secrets 里，**永远不进这个仓库**：不复制进来，不写进脚本或配置，也不贴进对话。
