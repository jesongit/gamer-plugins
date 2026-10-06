# gamer-yaml

自动化编辑器、函数库、模板和 YAML v2 WASM 解释器。插件 ID、目录名和发行归档均使用 `gamer-yaml`。

## 目录与开发

- `manifest.toml`：身份、版本、权限、依赖和面板声明的单一来源。
- `ui/`：独立 Vue/Vite 工程；`entry.js` 导出 SDK v1 接入契约及面板描述。
- `host/`：Rust 宿主适配代码，随 Core 编译；新原生函数、WIT 或资源校验契约变更需要更新宿主。
- `guest/`：可单独构建/安装的 WASM Component。
- `interpreter/`：YAML v2 解释器，guest 与宿主测试共用。

```powershell
# 仓库根目录执行；只打包本插件，其他市场条目保留
.\plugins\gamer-yaml\build.ps1
# 仅 UI 开发构建（同步到本地 web/public/plugin-ui 与已存在的 web-dist）
node sdk/ui/build-modules.mjs gamer-yaml
pnpm --dir plugins/gamer-yaml/ui test
```

依赖 Node 20+、pnpm、Rust；WASM 插件还需 `rustup target add wasm32-unknown-unknown`。构建脚本安装锁定的 UI 依赖并校验 .gplugin 的 ID、版本和 SHA-256。产物在 `web/public/plugins/`；在 Gamer「插件」页导入后保存当前编辑并刷新页面。

UI 与 WASM 可在现有宿主契约内独立更新，无需重编译主程序。`host/` 不是热加载 Rust 库，这部分改动需服务端构建。跨插件功能使用公开动作或共享消息通道；不能跨目录修改其他插件的数据。

工作台统一使用「自动化」入口，包含「脚本 / 函数 / 模板 / AI 生成与验证」。脚本和函数库使用无损原文编辑，必须声明 `version: 2`；函数页可选择库中函数进行测试运行。旧可视化模型不再重新序列化 v2 文档，保存保留注释和完整源码，并检查资源版本。

## 离线生成与验证

从视频工作台选择已有素材，或导入自包含 `.gamersample`，可以在没有设备的情况下建立候选。AI 生成需要可选的 gamer-ai 插件运行、模型配置及当前配置的视觉探测通过；「手写源码离线验证」、普通编辑、模板测试和运行历史不依赖 AI。

候选保留完整选中素材集，服务端通过同一引擎回放全部样本。报告区分通过、失败、证据不足与不支持；每份素材均通过且候选未被再次编辑才启用「确认保存正式版本」。保存前展示候选源码、模板来源与验证覆盖，用户确认后原子提交脚本、模板和版本记录；「保留候选草稿」不会写入正式资源。自动修正受次数、耗时与 Token 预算限制，支持取消，切包后的迟到结果不能污染新包。

运行详情新增图像证据：步骤截图、搜索框、命中框、Trace 开关、采集缺口、过期与失败前后两类帧。调用栈 `frame_id` 不等于 `image_id`。可以长期保留未过期的已完成运行证据，或将指定脚本/运行上下文交给 AI 只读分析。修改先形成候选，再经用户审核与验证保存。

语法与边界见 [YAML v2 参考](../../docs/reference/YAML.md)。真实设备和真实模型能力需另行验收；本地协议桩与组件测试不替代实测。

UI 与 Core 共享的模块清单在 `sdk/ui/host-modules.json`，构建时映射为宿主 SDK，不打包另一份 Vue/store。新增宿主 API 要先扩展并验证该契约。宿主 UI 声明 `ui.host` 权限，在安装确认中明确其页面访问能力；第三方需要隔离时使用原有 sandbox iframe SDK。

通用说明：[插件开发指南](../../docs/guides/plugin-dev.md)，[界面设计规范](../../docs/design/gamer-ui-spec.md)。

## 浏览器目标

配合提供 CDP 浏览器目标的 Gamer 宿主，原有截图识别、tap/swipe、key、input_text 与定时 runner 可用于无窗口页面；`key` 使用浏览器逻辑键名，Android 数字键码语义保留。匹配对象及 `center` 携带帧来源，导航或改绑后旧坐标会被拒绝。登录和进入游戏流程仍由 YAML 编排，launch/stop_app 仅用于 Android。此接入修改了 `host/`，必须同时更新宿主，单独更新插件归档不能给旧宿主增加 CDP 能力；公开 WIT/UI SDK 未变更。
