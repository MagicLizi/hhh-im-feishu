---
name: develop-plugin
description: 改飞书插件：修 bug、加或改协议方法、跟上 HHH 协议或宿主翻译的变化、补取样。用户说“修一下 xx”“支持 xx 消息”“协议加了 xx 字段”“HHH 那边翻译改了”时用。
---

# 改飞书插件

先读 `AGENTS.md`（规矩与仓库结构）和协议 `docs/protocol-v1.md`（在 hhh-im-sdk 仓库里）。第一原则是体验和内置飞书一样。

## 找地方

- 飞书接口、飞书格式怎么翻成统一类型：`src/feishu/`。平台事实和说明文字在 `src/feishu/facts.rs`。
- 协议方法：`src/server/mod.rs` 按方法名分派。握手声明在 `declare.rs`，会话和续期在 `session.rs`，登录在 `login.rs` 和 `relay.rs`，
  读取与动作在 `methods.rs`，资源与上传在 `resources.rs`。
- 统一类型翻成线上类型：`src/server/wire.rs`。它必须是宿主翻译的严格反向，宿主那一半在 `wire/host_inverse.rs`。

## 改一处

1. 先写测试再改代码：
   - 飞书那一侧：用 `tests/feishu_http.rs` 里的假飞书。
   - 协议那一侧：用 `tests/server.rs` 里的进程内宿主，宿主和插件按协议说话。
   - 翻译：用 `src/server/wire/goldens.rs`，内部 → 线上 → 宿主翻回来，结果要一字不差。
2. 新取样放进 `tests/fixtures/feishu/`，人名、编号、内容一律换成编的，输入和 `*.expected.json` 一起换。
3. 错误用协议的错误分类（`src/server/errors.rs`），给用户看的原因写人话，和内置时的说法一样。
4. 协议表达不了时，到 hhh-im-sdk 提 issue，不在插件里绕路。

## HHH 那边变了

- **协议加了字段**：重新构建，按编译错误一处处补上；能对齐内置时的就照内置写，再在 `declare.rs` 或 `wire.rs` 补测试。
- **宿主翻译改了**：照着 HHH 的 `hhh-im/src/provider/translate.rs`（表情在 `provider.rs`）同步 `host_inverse.rs`，再改 `wire.rs`，让往返重新一字不差。

## 每次改完

1. `cargo test`、`cargo fmt --check`、`cargo clippy --all-targets` 全部通过。
2. 样例要变时，用 `HHH_UPDATE_GOLDENS=1 cargo test` 重写 `tests/fixtures/feishu-protocol/`，逐个看差异，在交付说明里列出要拷进 HHH 的文件。
3. 打包并自检：`powershell -File scripts/pack.ps1`，再跑 `hhh-im-check dist/stage/hhh-im-provider.json`。
4. 在 HHH 里试：设置 → IM → 添加 IM → 文件夹，选 `dist/stage`；已经装过就在卡片里点「管理 → 从文件更新」。登录、读、发都走一遍。

## 不要

- 不在插件里存聊天记录、凭据或配置（都交给 HHH）。
- 不往标准输出写协议以外的东西（日志走 `log/write`）。
- 不改清单的 `id`（`feishu`）。
- 不把真实的人名、编号、公司内容写进夹具、测试或注释。
