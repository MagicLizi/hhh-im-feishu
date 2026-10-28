#!/bin/sh
# 打包（macOS / Linux）：编译、把程序与清单放进 dist/stage、用 hhh-im-pack 打成 .hhhim 并写版本索引。
# 用法：scripts/pack.sh [--sign-key <私钥文件>] [--publish]
#   --sign-key：用官方私钥给版本索引签名（飞书是官方插件，发版本必须签）；私钥在私有的密钥仓库
#               <hhh-keys>/official/hhh-official-signing.key，不放进这个仓库。
#   --publish：打好后发到清单 source 写的仓库；令牌放环境变量 HHH_IM_PUBLISH_TOKEN（或 GITHUB_TOKEN / GITLAB_TOKEN），不写进任何文件。
# 目标按这台电脑。注意：清单的 os 目前只写了 windows（只发 windows-x64 的包），要出 macOS / Linux 的包先把系统加进清单。
# hhh-im-pack 没装时：cargo install --git https://github.com/MagicLizi/hhh-im-sdk --tag v0.16.0 hhh-im-protocol --features transport,package
set -eu
cd "$(dirname "$0")/.."
cargo build --release
rm -rf dist/stage
mkdir -p dist/stage
cp hhh-im-provider.json logo.svg dist/stage/
cp target/release/hhh-im-feishu dist/stage/
hhh-im-pack dist/stage --out dist --notes notes.md "$@"
