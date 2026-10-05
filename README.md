# Gamer 官方插件

此仓库保存七款官方插件的 manifest、host、UI 和测试，其中 WASM 插件另含 guest。当前源码清单如下；源码版本与已发布目录分别管理。

| 插件 | 当前源码版本 | 执行类型 | 使用说明 |
| --- | --- | --- | --- |
| `gamer-yaml` 自动化 | `0.1.7` | WASM | [自动化](gamer-yaml/README.md) |
| `gamer-keymap` 键盘映射 | `0.1.5` | WASM | [键盘映射](gamer-keymap/README.md) |
| `gamer-video` 视频工作台 | `0.1.4` | builtin | [视频工作台](gamer-video/README.md) |
| `gamer-package-publisher` 配置包发布 | `0.1.0` | builtin | [配置包发布](gamer-package-publisher/README.md) |
| `gamer-live` 直播助手 | `0.2.5` | builtin | [直播助手](gamer-live/README.md) |
| `gamer-notify` 通知助手 | `0.1.0` | builtin | [通知助手](gamer-notify/README.md) |
| `gamer-ai` AI 助手 | `0.3.10` | builtin | [AI 助手](gamer-ai/README.md) |

源码迁自 `jesongit/gamer` 的 `689cc47b1fcaa2780fc97cd357db25f23da84ddd:plugins/`；迁移前历史保留在主仓。

## 独立构建（Windows）

安装 Rust stable（含 wasm32-unknown-unknown）、Node 24、各 UI packageManager 指定的 pnpm 和 PowerShell 7，然后运行：

```powershell
./build.ps1 -ChecksumsFile "$PWD/dist/sha256sums.txt"
# 只构建一个插件
./gamer-yaml/build.ps1
```

构建只需要本仓库，输出在 dist/；不要求兄弟目录存在 Gamer。sdk/ 是固定宿主提交导出的 WIT、UI 构建桥和打包工具快照，sdk/lock.json 记录来源和逐文件 SHA256，构建前强制校验。SDK v1 通过宿主提供的 globalThis.__gamerPluginSdkV1 绑定共享 UI 能力，不打包宿主源码。更新 SDK 使用固定 Gamer checkout 中的 tools/export-plugin-sdk.ps1，审查锁文件变化并执行两仓测试。

## 测试与联调

解释器测试可以独立运行：`cargo test --locked --manifest-path gamer-yaml/interpreter/Cargo.toml`。
UI 与 host 集成测试需显式检出 sdk/lock.json 指定的 Gamer 提交，并把本仓检出到其 plugins/ 目录；CI 的 integration job 固定执行此布局，不跟随 Gamer main。Keymap UI 目前无独立测试文件，不能计为覆盖。

主仓使用固定 gitlink 引用本仓。涉及 host/ 或 builtin 的变更仍需发布新版 Gamer；WASM/UI 可在声明的宿主 API 范围内独立发布。

## 发布

推送 `<plugin-id>-v<manifest-version>` tag 会先运行独立构建与固定宿主集成测试，再建立 Release 草稿；没有自动转正式发布。每次发布包含经过同一基线测试的完整插件快照及版本化 registry.json，各插件版本互不绑定。所有下载地址使用不可变 tag，不覆盖已有 Release 资产。

此前正式目录 [官方插件 0.1.0（含配置包发布）](https://github.com/jesongit/gamer-plugins/releases/tag/gamer-package-publisher-v0.1.0) 包含四款 `0.1.0` 归档，自动化、键盘映射和视频工作台复用原已公开字节。这份发行目录不表示当前七款源码均已发布。配置包发布插件要求 Gamer 0.2.1 的 resource 1.1 和 package.publish 能力；该目录的自动化要求 input 1.1（Gamer 0.2.0 起提供）。新能力请先升级本体，当前具体版本要求以各 manifest 为准。公开 beta 可正常升级，已有内部测试安装不自动降级。该次发行验收范围见 [发行说明](RELEASE_NOTES.md)。

[配置包发布插件](gamer-package-publisher/README.md) 使用运行 Gamer 的电脑上的 gh 登录创建草稿并确认公开；配置订阅和下载无需 gh 登录。默认公开配置仓库为 [gamer-packages](https://github.com/jesongit/gamer-packages)，也支持用户自己的公开 GitHub 仓库。

Gamer 启动器首次安装使用主仓发行锁指定的插件 Release，完整离线包携带同一份归档。软件插件页已接入独立发布目录发现：beta 宿主允许预发布，稳定宿主过滤预发布；手动刷新立即检查，网络失败回退缓存和随包目录。归档经宿主同源下载并校验大小/SHA256，安装或更新仍由用户确认，不自动覆盖已安装插件。

这里的最新源码和用户首装种子分别管理：README 等文档更新不代表已发布插件字节变化；主仓升级固定插件提交时，需要同步新的插件发布锁并完成两仓验收。

同版本插件通过 `release-reuse.lock.json` 固定旧归档来源与 SHA256；发布时逐 ZIP 条目比较本次构建和旧包，内容相同才复用已发布字节，内容变化必须升级版本并更新锁。

直播助手 `gamer-live` 的音视频输出与 B 站接入方式见 [使用说明](gamer-live/README.md)。它包含新的原生服务，需与对应 Gamer 本体一起发布；尚未加入已有发行的首装种子。

AI 助手 `gamer-ai` 提供通用视觉自动游玩与本机 MCP，支持 Responses 和 Chat Completions 显式选择。人工操作须先暂停 AI 并等待暂停完成；使用方法与令牌/控制租约说明见 [使用说明](gamer-ai/README.md)。它是 builtin 插件，新增宿主能力需要与匹配的 Gamer 版本一起发布。
