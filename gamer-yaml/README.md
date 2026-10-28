# gamer-yaml

自动化编辑器、函数库、模板和 YAML WASM 解释器。插件 ID、目录名和发行归档均使用 `gamer-yaml`。

## 目录与开发

- `manifest.toml`：身份、版本、权限、依赖和面板声明的单一来源。
- `ui/`：独立 Vue/Vite 工程；`entry.js` 导出 SDK v1 接入契约及面板描述。
- `host/`：Rust 宿主适配代码，随 Core 编译；新原生函数、WIT 或资源校验契约变更需要更新宿主。
- `guest/`：可单独构建/安装的 WASM Component。
- `interpreter/`：唯一 YAML 解释器，guest 与宿主测试共用。

```powershell
# 仓库根目录执行；只打包本插件，其他市场条目保留
.\plugins\gamer-yaml\build.ps1
# 仅 UI 开发构建（同步到本地 web/public/plugin-ui 与已存在的 web-dist）
node sdk/ui/build-modules.mjs gamer-yaml
pnpm --dir plugins/gamer-yaml/ui test
```

依赖 Node 20+、pnpm、Rust；WASM 插件还需 `rustup target add wasm32-unknown-unknown`。构建脚本安装锁定的 UI 依赖并校验 .gplugin 的 ID、版本和 SHA-256。产物在 `web/public/plugins/`；在 Gamer「插件」页导入后保存当前编辑并刷新页面。

UI 与 WASM 可在现有宿主契约内独立更新，无需重编译主程序。`host/` 不是热加载 Rust 库，这部分改动需服务端构建。跨插件功能使用公开动作或共享消息通道；不能跨目录修改其他插件的数据。

工作台保留脚本、工作稿、素材、模板和旧候选历史。脚本和函数库使用无损原文编辑，没有 YAML 语法版本字段。普通保存产生私有小版本，用户选择版本后发布正式大版本；工作稿修改不会自动发布或运行。

## 外部 Agent 与统一 MCP

内置模型生成、模型配置、候选自动纠错和自主编排已移除。用户自行选择成熟 Agent，通过宿主统一 MCP 调用插件 manifest 显式声明的语法/函数读取、工作稿编辑、截图素材检查、私有模板裁图和试跑工具。插件不包含模型请求包装，也不开放通用管理 action 透传。

现有源码、版本、素材和旧候选文件继续原位保留。旧候选可只读查看和逐字节分块导出，或显式导入新的工作稿；不会自动迁移、覆盖同名正式脚本或恢复中断模型任务。新保存保留已有历史字段和未知元数据。素材关联可省略继承，或显式清空关联；原始素材和历史图片不随之删除。

素材检查只证明语法、引用、原 PNG 与模板匹配，不依赖原视频、不做录像回放，也不证明真实设备执行效果。私有试跑绑定 MCP token，撤销或过期会取消并排空运行。自 0.3.28（宿主至少 0.5.9）起，`draft_publish_preview` 返回工作稿 head、源码与正式资源 SHA256；有独立 `plugin.gamer-yaml.publish` scope 的 Agent 可用 `draft_publish` 发布准确的未发布 head，复用原发布事务，保留全部历史与私有模板。宿主默认令牌（含历史设置页默认令牌）自动涵盖该新增 scope；显式受限令牌仍需准确授权。回退和删除仍在管理 UI。外部执行检查冻结程序的完整调用闭包，首版不允许通知外发、直播切换或直播内部协作；OCR 需要单独授权。

具体动作、参数、scope、旧候选导出/导入和试跑边界见 [工作稿与历史 API](host/GENERATION_API.md)。统一语法见 [YAML 参考](../../docs/reference/YAML.md)。插件归档需声明 tools 与实际 Schema；旧安装没有声明时保持不公开，不静默修改安装数据。真实设备、外部 Agent 与游戏效果仍需另行验收。

UI 与 Core 共享的模块清单在 `sdk/ui/host-modules.json`，构建时映射为宿主 SDK，不打包另一份 Vue/store。新增宿主 API 要先扩展并验证该契约。宿主 UI 声明 `ui.host` 权限，在安装确认中明确其页面访问能力；第三方需要隔离时使用原有 sandbox iframe SDK。

通用说明：[插件开发指南](../../docs/guides/plugin-dev.md)，[界面设计规范](../../docs/design/gamer-ui-spec.md)。

## 浏览器目标

配合提供 CDP 浏览器目标的 Gamer 宿主，原有截图识别、tap/swipe、key、input_text 与定时 runner 可用于无窗口页面；`key` 使用浏览器逻辑键名，Android 数字键码语义保留。匹配对象及 `center` 携带帧来源，导航或改绑后旧坐标会被拒绝。登录和进入游戏流程仍由 YAML 编排，launch/stop_app 仅用于 Android。此接入修改了 `host/`，必须同时更新宿主，单独更新插件归档不能给旧宿主增加 CDP 能力；公开 WIT/UI SDK 未变更。

## 脚本内函数

工作台取消独立函数页。脚本源码允许顶层 functions，新增函数保留其余原文；本脚本按函数名调用，同包其他脚本按 `common.yaml` 或 `common.yaml#函数名` 调用。参见主仓 docs/reference/YAML.md；运行、源码校验与离线验证使用相同的冻结定义。视频素材包来自独立的视频素材库，与脚本配置包分别管理。

## 直播输出设备切换

启用直播助手并开始输出后，在 `run` 中调用 `switch_live_device: {}` 即切换到当前脚本目标；也可指定 `{device_id: browser-example, timeout: 30s}`。支持 Android 和浏览器，保持同一素材地址与输出参数，新画面准备失败保留原输出。函数需要 `media.stream`，gamer-live 为可选依赖，不启用也可编辑和校验脚本，执行该函数时会明确报错。浏览器输出画面与静音，Android 沿用设备音频。完整契约见主仓 `docs/reference/YAML.md`。

## 按脚本结果通知

`notify` 默认立即发送。设置 `when: success` 或 `when: failure` 时，先登记当前脚本或函数的结果通知，在该作用域的步骤执行完毕或发生异常后发送。`enabled` 默认为 `true`，可引用布尔参数；关闭的结果通知也覆盖此脚本已有的全局或任务默认通知规则。模板/OCR/输入等步骤报错与解释器异常都会进入失败路径，通知插件缺失或发送失败不改变脚本的结果。被调用的脚本按自己的参数控制通知，同一次错误优先发送最具体的子作用域失败通知，避免外层脚本重复报告。通知自动附当前目标的配置名称，适用于 Android 和浏览器目标。

```yaml
params:
  notify_success: {type: boolean, default: true, desc: 成功是否通知}
  notify_failure: {type: boolean, default: true, desc: 失败是否通知}
run:
  - notify: {when: success, enabled: $notify_success, content: 日常执行成功}
  - notify: {when: failure, enabled: $notify_failure, content: 日常执行失败}
  - reset_input: {}
```

直播脚本调用 `{notify_success: false, notify_failure: true}`，即可保留失败通知并关闭日常成功通知。执行准备阶段尚未登记脚本策略的错误仍由已有全局结果规则处理。

结果通知没有显式指定 `channel` 时，先使用该脚本已保存的启用结果订阅规则中的通道，未配置脚本规则才使用全局默认通道。被调用脚本按自己的源码作用域匹配，保留原日常脚本选择的渠道，不修改用户的默认通道。
