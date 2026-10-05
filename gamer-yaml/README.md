# gamer-yaml

自动化编辑器、函数库、模板和 YAML V1 WASM 解释器。插件 ID、目录名和发行归档均使用 `gamer-yaml`。

## 目录与开发

- `manifest.toml`：身份、版本、权限、依赖和面板声明的单一来源。
- `ui/`：独立 Vue/Vite 工程；`entry.js` 导出 SDK v1 接入契约及面板描述。
- `host/`：Rust 宿主适配代码，随 Core 编译；新原生函数、WIT 或资源校验契约变更需要更新宿主。
- `guest/`：可单独构建/安装的 WASM Component。
- `interpreter/`：YAML V1 解释器，guest 与宿主测试共用。

```powershell
# 仓库根目录执行；只打包本插件，其他市场条目保留
.\plugins\gamer-yaml\build.ps1
# 仅 UI 开发构建（同步到本地 web/public/plugin-ui 与已存在的 web-dist）
node sdk/ui/build-modules.mjs gamer-yaml
pnpm --dir plugins/gamer-yaml/ui test
```

依赖 Node 20+、pnpm、Rust；WASM 插件还需 `rustup target add wasm32-unknown-unknown`。构建脚本安装锁定的 UI 依赖并校验 .gplugin 的 ID、版本和 SHA-256。产物在 `web/public/plugins/`；在 Gamer「插件」页导入后保存当前编辑并刷新页面。

UI 与 WASM 可在现有宿主契约内独立更新，无需重编译主程序。`host/` 不是热加载 Rust 库，这部分改动需服务端构建。跨插件功能使用公开动作或共享消息通道；不能跨目录修改其他插件的数据。

工作台统一使用「自动化」入口，内部提供「脚本 / 函数 / 模板」子页签；脚本与函数保留各自选择，切换时保护未保存修改，函数定义跳转与返回自动切换对应子页签。

0.1.8 修复模板重命名后已打开的可视化编辑器仍保存旧引用的问题：同步整个函数库及嵌套步骤，保留未保存修改、选择和撤销记录；保存前刷新模板候选，其他页面完成的重命名也能根据已保存快照与磁盘引用改写安全同步。仅在磁盘变化完全来自引用改写时更新保存版本，其他并发修改仍受冲突保护。模板替换导致区域/颜色后缀变化时使用同一通知流程，该流程需要宿主页面接线；更新后刷新页面生效。

缓存中的脚本/函数页不响应资源选择事件，避免函数保存刷新脚本列表时，隐藏脚本选择器清掉当前画布和撤销记录。

UI 与 Core 共享的模块清单在 `sdk/ui/host-modules.json`，构建时映射为宿主 SDK，不打包另一份 Vue/store。新增宿主 API 要先扩展并验证该契约。宿主 UI 声明 `ui.host` 权限，在安装确认中明确其页面访问能力；第三方需要隔离时使用原有 sandbox iframe SDK。

通用说明：[插件开发指南](../../docs/guides/plugin-dev.md)，[界面设计规范](../../docs/design/gamer-ui-spec.md)。

## 浏览器目标

配合提供 CDP 浏览器目标的 Gamer 宿主，原有截图识别、tap/swipe、key、input_text 与定时 runner 可用于无窗口页面；`key` 使用浏览器逻辑键名，Android 数字键码语义保留。匹配对象及 `center` 携带帧来源，导航或改绑后旧坐标会被拒绝。登录和进入游戏流程仍由 YAML 编排，launch/stop_app 仅用于 Android。此接入修改了 `host/`，必须同时更新宿主，单独更新插件归档不能给旧宿主增加 CDP 能力；公开 WIT/UI SDK 未变更。
