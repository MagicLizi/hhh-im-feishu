# 飞书（HHH IM 插件）

HHH 的飞书插件：在 HHH 里看飞书的群聊、私聊与话题，回消息、发图片和文件、撤回、点表情回应。

它是从 HHH 内置的飞书搬出来的独立插件，走 HHH 的统一插座 HHH-IM-Protocol v1。界面、登录方式和各种说明文字都和内置时一样，
本机已经存下的聊天记录和头像照样能用。装上后会接过内置飞书的登录，不用重新登录。

## 能做什么

- **登录**：两种方式，和内置的登录面板一样。
  - 通过中继：用中继上配置的飞书应用。
  - 使用自己的应用：填企业的飞书自建应用（App ID、App Secret），登录和续期都只在这台电脑上。
- **续期**：登录快过期时自动续期，换到的新授权先交给 HHH 存好再用。HHH 没在用这个插件时，也会按时叫它续期。
- **读取**：会话列表、搜群、搜人、找私聊、群成员、最近聊过的会话、消息与话题、表情回应、人员资料。
- **内容**：文字、提及、富文本、卡片（含表格、图片组、人员）、图片、文件、视频、表情包。
- **发送与动作**：回复、回到话题里、@某人与 @所有人、发图片和文件、撤回、加表情回应、去掉表情回应。
- **格式发送**：粗体、斜体、下划线、删除线、链接、行内代码、列表、引用和代码块；普通段落用原生表情。列表、引用中的表情在飞书 App 里仍是文字；同段同时有行内代码和表情时优先保留表情图，行内代码只保留文字。官方飞书仍不支持编辑自己的消息。
- **资源**：图片、文件、头像、表情图都分段交给 HHH。

聊天记录、凭据和配置都由 HHH 保存，插件自己不存。

## 构建

```sh
cargo build --release
cargo test
cargo fmt --check
cargo clippy --all-targets
```

程序是 `target/release/hhh-im-feishu`，Windows 上是 `hhh-im-feishu.exe`。Windows 上按窗口程序编译，HHH 拉起它时不弹控制台窗口。

格式发送使用公开 SDK `v0.14.0` 的 `Outgoing::Rich` 与 `capabilities.richSend`，本版需要带 IM SDK 0.14.0 或更新版本的 HHH。构建固定公开标签，不依赖本地源码替换。

## 自检

装自检和打包工具（格式发送自检需使用含上述新增能力的 SDK）：

```sh
cargo install --git https://github.com/MagicLizi/hhh-im-sdk --tag v0.14.0 hhh-im-protocol --features transport,package
```

打包后自检，全部通过才算改好：

```sh
powershell -File scripts/pack.ps1
hhh-im-check dist/stage/hhh-im-provider.json
```

## 在 HHH 里试

| 方式 | 什么时候用 |
|---|---|
| 设置 → IM → 添加 IM → 文件夹 | 开发时，选 `dist/stage` |
| 添加 IM → 安装包 | 选 `dist/` 里的 `.hhhim`，和用户装的一样 |
| 卡片「管理 → 从文件更新」 | 已经装过，换成新打的包或文件夹 |

## 发版本

发布在私有中控仓库 `MagicLizi/hhh-release` 里统一做（只有维护者能操作）：

- **让 agent 发**：在 HHH 仓库说“发布飞书插件”，agent 用 `hhh-im-release` skill 读改动、拟版本号和更新说明，你确认后一次发完。
- **自己点**：hhh-release 的 Actions →「插件发布（一键）」，选插件、填版本号和更新说明。

两种方式都会把版本号和说明合进 main，再编 Windows、M 芯片 Mac、Intel Mac 的包、核对、签官方签名、转正式。要出哪些系统写在 `.github/hhh-plugin.json`。
详细规矩见 [hhh-im-sdk 的发版本规范](https://github.com/MagicLizi/hhh-im-sdk/blob/main/docs/release.md)。

## 仓库里有什么

| 位置 | 内容 |
|---|---|
| `src/im/`、`src/net.rs`、`src/feishu/` | 从 HHH 搬来的飞书适配器：统一消息类型、统一 TLS 握手、飞书接口与翻译 |
| `src/server/` | 协议服务：握手声明、登录、续期、交接、读取与动作、资源与上传，以及统一类型和线上类型之间的翻译 |
| `tests/fixtures/feishu/` | 飞书响应的取样（人名、编号和内容都是编的），以及内置飞书翻出来的样子（`*.expected.json`） |
| `tests/fixtures/feishu-protocol/` | 协议样例：插件发给 HHH 的线上消息。这些样例会拷进 HHH，用宿主真正的翻译再核对一遍 |
| `scripts/pack.ps1`、`scripts/pack.sh` | 打包与发布 |
| `scripts/emoji-snapshot.mjs` | 重新生成随包的表情快照 |

## 许可证

[MIT](LICENSE)

## 图片的预览与原件

开放接口图片使用同一份平台文件供显示和保存；已用两份已有发送快照核对，消息资源接口返回的内容与上传字节一致。接口不提供可选择的缩略图档位，文件仍按原有读取。

新宿主经 `resources/describe` 选择实际版本，读取时带 `variant`；省略 `variant` 的旧调用仍按原来的方式读取；二进制安装仍需通过宿主的最低 SDK 检查。分段缓存包含实际版本，不把预览当成原件。本版固定使用 IM SDK v0.14.0，宿主需支持该版 SDK。
