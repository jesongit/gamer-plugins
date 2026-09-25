# Gamer 官方插件

此仓库保存 gamer-yaml、gamer-keymap、gamer-video 的 manifest、host、WASM guest、UI 和测试。
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

推送 `<plugin-id>-v<manifest-version>` tag 会先运行独立构建与固定宿主集成测试，再建立 Release 草稿；没有自动转正式发布。每次发布包含经过同一基线测试的三插件快照及版本化 registry.json，各插件版本互不绑定。所有下载地址使用不可变 tag，不覆盖已有 Release 资产。

正式发行基线为 [官方插件 0.1.0](https://github.com/jesongit/gamer-plugins/releases/tag/gamer-yaml-v0.1.0)，配套 [Gamer 0.2.0](https://github.com/jesongit/gamer/releases/tag/v0.2.0)。自动化要求宿主 input 1.1，旧本体拒绝安装；请先升级本体。自动化、键盘映射和视频工作台均为 0.1.0。公开 beta 可按正常版本顺序升级；内部开发版本号不自动降级，已有内部测试安装请保留配置数据后卸载旧插件，再安装公开版本。验收范围与尚未完成的真机补充测试见 [发行说明](RELEASE_NOTES.md)。

Gamer 启动器首次安装使用主仓发行锁指定的插件 Release，完整离线包携带同一份归档。软件插件页已接入独立发布目录发现：beta 宿主允许预发布，稳定宿主过滤预发布；手动刷新立即检查，网络失败回退缓存和随包目录。归档经宿主同源下载并校验大小/SHA256，安装或更新仍由用户确认，不自动覆盖已安装插件。

这里的最新源码和用户首装种子分别管理：README 等文档更新不代表已发布插件字节变化；主仓升级固定插件提交时，需要同步新的插件发布锁并完成两仓验收。

同版本插件通过 `release-reuse.lock.json` 固定旧归档来源与 SHA256；发布时逐 ZIP 条目比较本次构建和旧包，内容相同才复用已发布字节，内容变化必须升级版本并更新锁。
