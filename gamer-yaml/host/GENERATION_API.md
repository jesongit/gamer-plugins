# 工作稿、素材检查与旧候选历史 API

内置模型调用、提示词、图像模型包装、候选纠错循环和自主编排已移除。外部 Agent 通过宿主统一 MCP 调用下列显式工具；插件不保存模型配置、不代替 Agent 生成源码。

管理 UI 仍使用 `POST /api/extensions/gamer-yaml/call`，请求为 `{action,values}`，插件必须处于 Running。MCP 通过独立的 `ExternalToolHandler` 入场，不能把管理 action 作为通用插件调用透传。

## 工作稿与版本

`authoring.list {package_id}` 返回脚本名、工作稿 head 和正式版本标记；`authoring.get {package_id,script_id}` 返回完整源码历史及保留的历史元数据。私有模板字节通过独立的模板读取动作获取。

- `authoring.create {package_id,script_id}`：建立空工作稿。已有同名工作稿或正式脚本时拒绝。
- `authoring.import {package_id,script_id}`：用户显式把已存在正式脚本导入工作稿，保留正式发布历史锚点；不发布、不覆盖正式脚本。
- `authoring.save {package_id,script_id,expected_version,yaml,version?,instruction?,material_refs?,template_refs?}`：原文保存一个小版本。`expected_version` 必须是当前 head，空工作稿用空字符串；`version` 可选择继承素材和模板的旧版本，省略时取 head。
- `authoring.validate {package_id,script_id,expected_version,version?}`：语法、函数/模板引用及可选截图素材匹配检查。无素材也能编辑和检查普通逻辑；报告明确区分 `syntax_and_references` 与 `screenshot_templates`。
- `authoring.run {package_id,script_id,expected_version,version?,device_id,args?}`：冻结选中版本、函数源码和模板后，提交私有版本试跑，返回 `run_id`。真实输入仍经过 Core 运行槽、活动租约和输入屏障。

`material_refs` 的元素为 `{plugin_id,sample_id,content_sha256,candidate_id?}`。独立素材库使用 `plugin_id:"gamer-video"`；旧候选内冻结素材使用 `plugin_id:"gamer-yaml"` 并携带 `candidate_id`。宿主核对真实归属、素材身份与内容指纹，不接受把调用者声明的 Package 当作独立素材归属。

`template_refs` 是当前 Package 的正式模板名。显式传入时，宿主冻结实际 PNG 到私有版本。两个引用字段省略时继承，传空数组时清除关联；不可变旧版本和已嵌入私有图片仍保留，原素材/正式模板不被删除。

工作稿文件继续使用 `extension-data/gamer-yaml/authoring/<package>/<script-hash>.json`。原 `source`、`usage`、`job`、版本编号、图片和未知元数据在新保存时保留。旧未完成 job 只在读取响应中显示 interrupted，不恢复模型任务、不重写历史文件。

## 私有模板

- `authoring.template {package_id,script_id,expected_version,version?,name}`：读取选中版本可用的模板 PNG、原尺寸和 SHA-256。
- `authoring.crop_template {package_id,script_id,expected_version,version?,name,sample_ref,frame_id,rect}`：从经验证的原始素材 PNG 按整数 `[x,y,width,height]` 裁图，保存一个新私有小版本。坐标以原 PNG 像素为准，预览缩放不改变坐标。同名不同字节冲突，须另取模板名；不会写正式模板。
- `authoring.match_template {package_id,script_id,expected_version,version?,name,sample_ref,frame_id,threshold?,region?}`：对一个原始 PNG 做模板匹配，返回 `single_screenshot_template` 报告，不获取设备输入租约。

`sample_ref` 与 `material_refs` 的元素使用相同身份格式。素材只包含原始截图和操作事件；读取、裁图和检查不依赖原视频，不做录像回放。单张图的匹配结果不能证明未见分支或真实设备运行效果。

## 人工发布与删除

管理 UI 保留 `authoring.publish`、`authoring.delete_preview`、`authoring.delete`。正式发布由用户选择版本完成，工作稿更新不会自动发布。删除按既有工作稿所有权规则处理；正式脚本、正式模板、发布历史、共享素材及无法确认归属的记录继续保留。

MCP 不声明发布、回滚、删除工作稿、任意资源写入或模型设置工具。`authoring.generate` 已移除。

## 旧候选：只读、原样导出、显式导入新工作稿

旧 JSON 与 `.snapshot` 保持原路径、原字节，不进行自动迁移或自动执行：

- `generation.list {package_id}` 返回 `{candidates,legacy_read_only:true}`。
- `generation.get {package_id,candidate_id}` 返回原元数据 envelope 和只读标记，保留未知字段。原 generating/validating 状态只在响应中投影为 retired，并附 `legacy_original_state`；不改变文件、旧用量或校验报告。
- `generation.template {package_id,candidate_id,name,pending?}` 只读重建旧截图裁图/冻结模板；`pending:true` 只预览已保存的旧提案，不接受或修改提案。
- `generation.history {package_id}` 返回已有发布事务历史；读取不触发恢复或回滚。
- `generation.export {package_id,candidate_id,part?,offset?,max_bytes?}` 分块读取原文件。`part` 为 metadata（默认）或 snapshot；默认及最大块大小 256 KiB。返回 base64、块 SHA-256、offset、total_bytes、next_offset、complete，客户端按偏移拼接可得到逐字节相同的文件。
- `generation.import_draft {package_id,candidate_id,new_script_id}` 显式建立全新工作稿，返回 `{script}`。同名工作稿或正式脚本一律拒绝；新版本从 0.1 开始，不继承正式发布锚点。复制候选源码、冻结函数、模板和实际素材引用，并保留完整候选来源元数据；不修改旧候选或正式资源。

`generation.readiness/create/start/repair/edit/apply_proposal/validate/retry/cancel/save/rollback` 已退出动作目录并被拒绝。无任何旧模型任务恢复路径。确定性 `automation.validate_source {package_id,yaml,kind?}` 仍用于源码编辑检查。

启动时只保留既有正式发布事务 journal 的恢复；它不遍历重写旧候选，也不重新提交已结束或中断的任务。

## 统一 MCP 工具

新插件归档通过 manifest `[[tools]]` 与归档内 JSON Schema 声明显式动作，完整名称为 `gamer.plugin.gamer-yaml.<tool_id>`。旧安装版本没有 tools 声明时不会自动公开，安装数据不会被静默升级。

| tool_id | 能力 | 必要业务 scope |
| --- | --- | --- |
| grammar / functions / draft_list / draft_get / draft_diff / template_read | 语法、函数、工作稿和模板读取 | plugin.gamer-yaml.read |
| draft_create / draft_import / draft_save / draft_crop | 私有工作稿修改 | plugin.gamer-yaml.draft.write + plugin.gamer-yaml.read |
| draft_validate / draft_match | 确定性素材检查 | plugin.gamer-yaml.validate + plugin.gamer-yaml.read |
| draft_run | 私有试跑 | plugin.gamer-yaml.draft.run + plugin.gamer-yaml.read + run.submit |
| legacy_list / legacy_get / legacy_export / history | 旧候选和发布历史读取 | plugin.gamer-yaml.history.read |
| legacy_import | 旧候选导入全新工作稿 | plugin.gamer-yaml.draft.write + plugin.gamer-yaml.read + plugin.gamer-yaml.history.read |

MCP `draft_get` 专用参数 `{package_id,script_id,offset?,limit?}` 默认每页 5 个版本，最多 20 个；也可单独传 `version` 获取一个确切版本。不能混用版本选择与分页字段。响应保留未知文档/版本元数据并提供 `total`（全部历史版本数）、`next_offset`、`complete` 和 `selected_version`；指定版本时 complete 表示该选中版本已完整返回。管理 `authoring.get` 仍返回全部历史。源码不静默截断：新保存上限为 UTF-8 512 KiB，历史读端不改写或丢弃更长旧源码；本版没有字符分页，可用单版本读取避免一次返回多份大源码。

`template_read {package_id,name}` 读取正式模板；读取工作稿私有模板时须同时提供 `script_id` 与 `expected_version`，可另带 `version` 选择历史版本。正式模板读取不接受版本选择字段。

所有工具核对 allowed Package/plugin；独立 gamer-video 素材额外要求 `media.read` 与精确 sample_id（或用户显式授予的通配范围）。私有试跑还要求允许设备和 runner `gamer-yaml-version`。产生副作用的 MCP 调用必须携带独立 `operation_id`；JSON-RPC id 仅用于响应关联。

私有试跑登记到调用 token，撤销、过期或登记失败时 cancel 并等待真实终态和租约清理。正式 Core run/task 的已接受委托独立存在，不借此恢复内置自主流程。

外部执行 policy 由宿主类型化传入，客户端 JSON 不能伪造。运行前检查同一份冻结程序的完整可达函数/脚本闭包：首版拒绝 notify、媒体外发/直播切换与直播内部协作；OCR 还要求 Core `ocr.recognize` scope。管理 UI 的正式执行继续按原权限和输入屏障运行。

本地合成回归验证协议、数据保留及授权边界；真实设备、外部 Agent 端到端和实际游戏效果另行验收。
