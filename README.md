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

用户市场切换到本仓在线目录应在首个 Release 正式发布、下载及安装验收后进行；迁移期间主仓仍保留随包的已验证目录，避免空仓导致市场不可用。
