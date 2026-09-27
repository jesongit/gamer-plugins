# 直播助手

提供设备音视频输出、直播互动接入，以及弹幕／礼物触发函数或自动化的串行队列。互动执行需要 Gamer 0.2.4+ 和已启用的 gamer-yaml；插件版本 0.2.3。

## 配合直播姬

1. 安装包含 `gamer-live` 原生实现的 Gamer 本体，再安装并启用本插件。
2. 在「直播助手」选择设备，保留「直播姬多媒体素材」，点击「开始输出」。
3. 复制本机素材地址，在同一台电脑的直播姬添加「多媒体」素材，粘贴地址。
4. 麦克风、场景、直播分区及正式开播由直播姬管理。

地址指向 Gamer **服务端所在电脑**的 `127.0.0.1`，每次启动输出都会更换，不适用于远程直播姬。本地素材使用 HLS，通常有数秒缓冲；首版不提供低延迟预览协议。设备静止时保持最近画面，设备不提供声音时输出静音。关闭插件面板不停止输出；「停止输出」、停用插件、卸载插件或退出服务端会清理进程和临时素材。

需要直接推流时，可选择「手动 RTMP / RTMPS」，填写完整服务器地址和推流码组成的 URL。点击开始即发送设备音视频，插件不采集麦克风，不自动获取平台推流码，也不代替平台的开播授权。目标地址仅在进程内使用，状态响应不返回它。

要求 FFmpeg 包含 H.264 解码、libx264、Opus 解码、AAC 编码、Matroska、HLS 和 FLV 支持。视频采用持续解码与定时编码，存在 CPU 开销；负载较高时降低帧率、设备分辨率或码率。一个插件实例同时支持一路音视频输出和一个互动账号。

## B 站互动

两种方式独立，必须使用对应平台批准的凭据：

| 方式 | 填写内容 | 说明 |
|---|---|---|
| 直播开放平台 | Access Key ID / Secret、应用 ID、主播身份码 | 对接 OpenLive `/v2/app/start`、心跳、结束与长连 |
| 开放平台 OAuth | Client ID（填入 Access Key ID）、App Secret、Access Token | 应用需有 `LIVE_ROOM_DATA` 等权限，主播需授权；对接 `/arcopen/fn/live/room/ws-start` |

开发者账号不等于直播接口权限。OAuth Token 需从自己的授权流程取得和更新；首版不内置 OAuth 回调服务或共享开发者密钥。每位 Gamer 使用者填写自己的凭据。点击「保存接入配置」或「保存并连接互动」会将当前接入方式的配置持久化到服务端；断开、停用或重启后仍保留。页面自动回填接入方式、Access Key ID、应用 ID，Secret、Token 和身份码显示「已保存，留空沿用」，不会把明文传回页面。更换 Access Key ID 不会沿用原账号的秘密，身份码同时绑定原应用 ID；OpenLive 与 OAuth 分别保存，互不混用。

Windows 使用当前系统账号的 DPAPI 加密文件，位于 `extension-data/gamer-live/private/connection.dat`；不能直接搬到另一系统账号使用。Unix 使用仅属主可访问的目录（0700）及文件（0600），该平台文件内容未加密。不写入配置包、浏览器存储或日志。「清除已保存配置」只移除当前接入方式，保存失败或版本冲突会显示错误并保留表单输入。服务重启后点击连接即可使用已保存配置，不自动开播或自动恢复网络连接。

连接具有 API 心跳、WebSocket 心跳、授权超时、断线退避重试和可见错误。OpenLive 主动断开调用结束接口；OAuth 断开关闭长连并停止心跳，由平台回收会话。平台下发哪些消息取决于权限、直播状态和事件实际发生情况。

## 互动规则与执行队列

三个页签：**直播设置**（音视频输出、互动账号连接、执行设备和配置包）、**互动规则**（自定义条件、执行入口、每条规则开关）、**触发日志**（弹幕接收、匹配原因、排队与运行结果）。

在「直播设置」选择执行设备和配置包，先为设备选择 Android 应用。添加常用示例后选择配置包中已有的函数或自动化，填写参数并保存。6 个示例默认停用，不内置游戏坐标：弹幕跳跃、放技能、换角色、开始挑战，以及礼物放技能、触发挑战。

规则支持弹幕等于／包含、礼物 ID 与最低数量。参数可取固定值、入口默认值或事件字段；礼物 ID／观众 ID 为字符串，礼物数量为整数。按列表顺序只处理首条命中的启用规则，冷却从成功入队开始；冷却或参数错误不会绕到下一条规则。普通弹幕触发，镜像弹幕只展示。

规则保存后，点击该规则的开关立即生效，无需再保存开关或开启总开关。新消息匹配后自动排队，前项结束后执行下一项；关闭规则只影响后续触发，已排队项可在「触发日志」中移除。

先用「仅预览匹配」检查参数；「加入队列测试」会生成标记为模拟测试的真实队列项并自动执行。队列明确绑定目标，切换页面不会改派。

每条礼物消息只入队一次，数量作为参数，不展开成多个运行。适配沿用官方 `SEND_GIFT.gift_num` 字段，不推测连送累计差值。当前有官方格式契约测试，真实账号连送仍需实际样本验证；使用前先对照最近消息和预览结果。依据：[官方 SendGift 定义](https://raw.githubusercontent.com/bilibili-openplatform/OpenLive_CSharpDemo/main/OpenBLive/Runtime/Data/SendGift.cs)。

| 操作 | 效果 |
|---|---|
| 每条规则开关 | 立即保存开关；开启后自动触发，关闭后不接受该规则的新事件 |
| 移除／清空等待项 | 只处理尚未启动的项，保留移除结果 |
| 取消当前运行 | 请求取消；等运行真正终止后才推进 |
| 清除执行目标 | 仅在当前／等待项均已处理后允许 |
| 重新执行 | 失败、取消或已核对项生成新项，排在队尾；不能改派其他目标 |

函数和自动化共用一条 FIFO，等待设备上的手动／定时运行结束，提交仍经过 Core 设备互斥。单项失败记录后继续；设备或公共依赖不可用暂停队列，修复后手动继续。可设单项超时（默认关闭），超时只请求取消，不强行启动后项。关闭页面不停止队列；断开直播只停收消息；停用插件清空等待并取消当前互动；正常服务退出保留等待项。

规则写入 `packages/<pkg>/plugins/gamer-live/interaction/rules.json`，资源版本冲突拒绝覆盖。队列写入本机 `extension-data/gamer-live/queue.json`，不随配置包导出、不含凭据。重启时若没有遗留任务，规则开关原样保留，连接直播后自动工作。若有遗留等待或活动项，日志页提示核对恢复；未确认的运行进入「结果待核对」，不得自动重跑。用户查阅日志核对后结束该项，再决定是否重新入队。

等待最多 100 项，满时拒绝新项并记录原因。历史／触发记录保留最多 7 天、各 10000 条，队列文件上限 16 MiB 时提前清理旧记录；等待和待核对项不自动过期。稳定事件 ID 在同一会话内去重（最多 7 天、20000 个），手动测试／重试请求 ID 在对应队列记录保留期间幂等。无事件 ID 不按相同正文合并，也不能识别平台重复投递。源码不做快照，队列固定入口、参数和目标，执行时使用当时保存的函数内容并重新校验。

## 动作与扩展边界

统一通过 `POST /api/extensions/gamer-live/call` 调用，插件必须处于 Running，能力清单来自 `/api/extensions/gamer-live/capabilities`。

| action | values | 权限 |
|---|---|---|
| `live.status` | `{}` | 无额外权限，仍需认证和 Running |
| `stream.start` | `{device_id, mode:"local"或"rtmp", push_url?, audio?, fps?, bitrate_kbps?}` | `media.stream` |
| `stream.stop` | `{}` | `media.stream` |
| `connection.connect` | `{platform_id:"bilibili", credentials:{mode,access_key,access_secret?,app_id?,identity_code?,access_token?},expected_version?}` | `live.connect` |
| `connection.disconnect` | `{}` | `live.connect` |
| `connection.settings.read` | `{}` → `{version,mode,profiles}`；profiles 只含公开字段及秘密存在标记 | `live.connect` |
| `connection.settings.save` | `{credentials,expected_version}`；秘密留空沿用同模式同账号的已保存值 | `live.connect` |
| `connection.settings.clear` | `{mode,expected_version}`；删除该模式的配置 | `live.connect` |
| `events.read` | `{after:0}`，后续传上次 `next_seq` | `live.connect` |
| `rules.read` | `{package_id}` → `{schema_version,rules,version}` | `resource.read` |
| `rules.save` | `{package_id,expected_version,ruleset:{schema_version:1,rules}}` | `ui.host` + `run.submit` |
| `rules.toggle` | `{package_id,id,enabled,expected_version}`，只更新指定规则开关 | `ui.host` + `run.submit` |
| `rules.preview` | `{kind:"message"或"gift",payload:{text?,gift_id?,count?}}` | `resource.read` |
| `queue.status` | `{offset?:0,filter?:"failed"}`，历史每页 30 项 | 无额外权限 |
| `queue.configure` | `{device_id,package_id}` | `run.submit` |
| `queue.test` | 同 preview，另需唯一 `request_id` | `run.submit` |
| `queue.control` | `{op,ids?:[],request_id?}`；op 为 resume/remove/clear/cancel/stop/resolve/retry/unbind（resume 仅用于异常恢复，stop 用于插件停止生命周期）；retry 需 request_id | `run.submit` + `run.control` |

事件协议 v1：`schema_version, seq, platform_id, connection_id, room_id, event_id, kind, occurred_at, received_at, actor, payload, platform_data`。统一种类含弹幕、礼物、醒目留言、舰长、点赞、进入和房间状态；原始平台字段保留在 `platform_data`。未知种类返回 `platform.event`，缺失身份返回 null，不猜测 UID；不把礼物价格当作收益。事件读取是有界的进程内观察缓存，不是可靠任务队列：最近 500 条，每次最多 100 条，单事件上限 64 KiB，同连接消息 ID 去重；落后于缓存时 `gap=true`。此观察缓存不保存完整消息历史、不发送弹幕；触发摘要与运行队列按上文单独保存。

平台适配位于 `host/bilibili.rs`，事件归一化位于 `host/events.rs`，插件生命周期与状态位于 `host/mod.rs`；通用音视频输出归 Core `media/output.rs`。后续平台扩展增加平台适配与字段映射，沿用同一输出机制和事件信封，无须将平台接口写入 Core。本插件是 builtin，变更 `host/` 或 Core 必须随新版 Gamer 发布；`.gplugin` 只包含 manifest 和 UI。

## 验证

UI：`pnpm --dir plugins/gamer-live/ui test`。

宿主：`cargo test --locked --no-default-features --manifest-path server/Cargo.toml extensions::live`。

真实 FFmpeg 合成素材验证：`cargo test --locked --no-default-features --manifest-path server/Cargo.toml media::output::tests -- --include-ignored`。此测试不连接设备、不公开直播。真实账号权限、直播姬 GUI 导入效果仍需在实际使用环境验证。

协议依据：[B 站开放平台文档](https://open.bilibili.com/doc)、[长连信息](https://open.bilibili.com/doc/4/da2b13dc-7f7a-0025-be11-0b677e793baa)、[长连协议](https://open.bilibili.com/doc/4/5cac94fe-57f9-06db-7515-523d81c44f85)、[签名规范](https://open.bilibili.com/doc/4/8673959e-f7bb-56e6-6e68-d225f971b81b)、[OpenLive 官方示例](https://github.com/bilibili-openplatform/OpenLive_CSharpDemo)。
