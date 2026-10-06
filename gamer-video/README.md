# gamer-video

素材、录制、时间轴、项目与视频制作界面。插件 ID、目录名和发行归档均使用 `gamer-video`。

## 使用流程

- 预览与制作：录制或导入 → 选中视频 → 创建制作项目 → 逐帧定位 → 框选模板/离线测试，或标记后保存项目
- 演示素材：录制时在投屏操作 → 结束录制 → 历史「制作素材包」→ 选择 START 与 END → 查看真实帧 → 填写目标并明确确认 END → 制作素材包
- 导入导出：演示素材页支持 `.gamersample` 导入、导出。素材内自带原尺寸 PNG、事件、时序、分段及完整性清单；默认附带原视频片段，等待期间由回放按需取帧
- 脚本生成和全素材验证归自动化插件。视频插件不生成或保存 YAML，不写自动化插件的私有目录
- 清理：删除项目或移除引用并保存 → 删除素材 → 确认删除录制历史。已导出的演示素材自包含，不依赖原媒体库

停止录制只代表捕获结束，绝不自动确认任务成功。制作时标注“证据完整”也不等于脚本已验证通过。输入事件丢失、视频损坏、分段间隙、长按/多指/非线性手势、坐标变更或采样过稀会给出无法验证诊断。没有真实设备和模型参与的测试不能替代实际游戏验证。

切换视频内部页签保留当前素材编辑；切换录制或配置包时会清除旧的画面确认，防止把旧来源提交到新上下文。模板制作能力继续通过 gamer-yaml 公开动作提供，基础录制与素材制作不要求 gamer-yaml 或 AI 运行。

## 可携带素材契约 v1

`host/sample.rs` 定义唯一格式；ZIP 内为 `manifest.json`、`frames/*.png` 和可选 `clips/*.mp4`。PackageStore 仅在视频命名空间存储 `samples/<id>.gamersample`，导入到另一个配置包不改素材身份或内容指纹。

- Manifest 包含 `schema_version=1`、`id/name/content_sha256/recording_id`、`start/end {timeline_us,frame_id}`、`goal {description,confirmed}`、`coordinates {space,width,height,rotation}`、`status/diagnostics/max_frame_gap_us`、`actions/frames/windows/segments/files`
- 帧身份保存原始媒体 SHA256、真实展示帧索引/PTS、会话时间及未缩放图像尺寸/方向；`timeline_us = segment.start_us + frame.pts_us`。Android 原始采集 `base_pts_us` 只保留供诊断，MP4 文件内 PTS 从零起，禁止把原始偏移再加回文件 PTS
- `actions` 使用 Core InputEventRecord；tap/swipe/key 保存实际 `duration_us`，swipe 保存 UP 端点及有界轨迹。缺失/不支持的手势保留显式类型，不伪造成完整 tap
- 每个动作的 before/after 窗口引用真实、不同时间的源帧；不能引用后一个动作发生后的结果来证明前一个动作。仅图片模式的全部观察帧间隙上限固定 500ms；有完整原视频的素材允许稀疏 PNG，但回放必须先核验原视频的实际时间范围与几何，再按需取帧，不能拿缺失视频放宽校验
- 内容指纹为把 `content_sha256` 置为空字符串后的 manifest 递归按对象键排序、紧凑 JSON 编码，再计算 SHA256；文件 SHA256 和长度均包含在 manifest 内。ZIP 时间戳固定、文件按路径排序，同一 manifest 与文件集合的输出逐字节一致
- 上限：128 MiB 压缩/展开总量，260 个文件，2 MiB manifest，240 张 PNG，100 个动作，5 分钟范围；PNG 解码限制 8192 边长、16,777,216 像素和 64 MiB 解码分配。附带视频时只采集动作及边界 PNG，较长等待复用视频按需解码；不附视频时长区间受帧数预算限制可能缺证据。原视频目前按原录制分段原样附带，不重新编码或假装已裁成所选区间；若整段超过大小限制，应缩短录制或分段制作
- 所有导入与读取校验路径、重复条目、符号链接、文件集合、长度/SHA、实际 PNG 解码、边界、坐标、窗口及时间映射。不会解压到任意磁盘路径

公开动作（经 `/api/extensions/gamer-video/call`）：

- `sample.create`：`{package_id,recording_id,name,start_us,end_us,goal,include_clips?}` → `{manifest,path,version}`
- `sample.list`：`{package_id}` → `{samples:[{id,name,status,content_sha256,path,diagnostics}]}`
- `sample.read`：`{package_id,sample_id}` → `{manifest,files:[{path,base64}]}`，浏览器只收到 PNG 预览；原视频留在服务端归档内。自动化通过包内归档引用取得完整证据，纯 `read_bundle` 为服务端回放返回 PNG 与视频字节，绝不把视频送进模型提示词
- 原始导入/导出复用 PackageStore 资源 PUT/GET；重复 ID 默认拒绝覆盖。公共纯格式函数 `validate_archive/read_bundle/validate_bundle` 可被自动化离线验证复用，解析已授权的素材字节不依赖安装或运行视频插件

## 目录与开发

- `manifest.toml`：身份、版本、权限、依赖和面板声明的单一来源。
- `ui/`：独立 Vue/Vite 工程；`entry.js` 导出 SDK v1 接入契约及面板描述。
- `host/`：Rust 宿主适配代码，随 Core 编译；新原生函数、WIT 或资源校验契约变更需要更新宿主。
- 视频执行类型是 builtin；媒体和录制机制由 Core 提供，无占位 WASM。

```powershell
# 仓库根目录执行；只打包本插件，其他市场条目保留
.\plugins\gamer-video\build.ps1
# 仅 UI 开发构建（同步到本地 web/public/plugin-ui 与已存在的 web-dist）
node sdk/ui/build-modules.mjs gamer-video
pnpm --dir plugins/gamer-video/ui test
```

依赖 Node 20+、pnpm、Rust；WASM 插件还需 `rustup target add wasm32-unknown-unknown`。构建脚本安装锁定的 UI 依赖并校验 .gplugin 的 ID、版本和 SHA-256。产物在 `web/public/plugins/`；在 Gamer「插件」页导入后保存当前编辑并刷新页面。

UI 与 WASM 可在现有宿主契约内独立更新，无需重编译主程序。`host/` 不是热加载 Rust 库，这部分改动需服务端构建。跨插件功能使用公开动作或共享消息通道；不能跨目录修改其他插件的数据。

UI 与 Core 共享的模块清单在 `sdk/ui/host-modules.json`，构建时映射为宿主 SDK，不打包另一份 Vue/store。新增宿主 API 要先扩展并验证该契约。宿主 UI 声明 `ui.host` 权限，在安装确认中明确其页面访问能力；第三方需要隔离时使用原有 sandbox iframe SDK。

通用说明：[插件开发指南](../../docs/guides/plugin-dev.md)，[界面设计规范](../../docs/design/gamer-ui-spec.md)。
