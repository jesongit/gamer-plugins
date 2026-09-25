# 直播助手

提供设备音视频输出和直播互动接入，不包含互动玩法或自动游戏操作。

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

开发者账号不等于直播接口权限。OAuth Token 需从自己的授权流程取得和更新；首版不内置 OAuth 回调服务或共享开发者密钥。每位 Gamer 使用者填写自己的凭据。连接成功后表单清除 Secret、Token 和身份码，服务端仅在当前连接任务内保存，断开/停用后释放，不写入配置包、浏览器存储或日志。服务重启不自动恢复带凭据的连接。

连接具有 API 心跳、WebSocket 心跳、授权超时、断线退避重试和可见错误。OpenLive 主动断开调用结束接口；OAuth 断开关闭长连并停止心跳，由平台回收会话。平台下发哪些消息取决于权限、直播状态和事件实际发生情况。

## 动作与扩展边界

统一通过 `POST /api/extensions/gamer-live/call` 调用，插件必须处于 Running，能力清单来自 `/api/extensions/gamer-live/capabilities`。

| action | values | 权限 |
|---|---|---|
| `live.status` | `{}` | 无额外权限，仍需认证和 Running |
| `stream.start` | `{device_id, mode:"local"或"rtmp", push_url?, audio?, fps?, bitrate_kbps?}` | `media.stream` |
| `stream.stop` | `{}` | `media.stream` |
| `connection.connect` | `{platform_id:"bilibili", credentials:{mode,access_key,access_secret,app_id?,identity_code?,access_token?}}` | `live.connect` |
| `connection.disconnect` | `{}` | `live.connect` |
| `events.read` | `{after:0}`，后续传上次 `next_seq` | `live.connect` |

事件协议 v1：`schema_version, seq, platform_id, connection_id, room_id, event_id, kind, occurred_at, received_at, actor, payload, platform_data`。统一种类含弹幕、礼物、醒目留言、舰长、点赞、进入和房间状态；原始平台字段保留在 `platform_data`。未知种类返回 `platform.event`，缺失身份返回 null，不猜测 UID；不把礼物价格当作收益。事件读取是有界的进程内观察缓存，不是可靠任务队列：最近 500 条，每次最多 100 条，单事件上限 64 KiB，同连接消息 ID 去重；落后于缓存时 `gap=true`。不保存消息历史、不发送弹幕、不解释玩法。

平台适配位于 `host/bilibili.rs`，事件归一化位于 `host/events.rs`，插件生命周期与状态位于 `host/mod.rs`；通用音视频输出归 Core `media/output.rs`。后续平台扩展增加平台适配与字段映射，沿用同一输出机制和事件信封，无须将平台接口写入 Core。本插件是 builtin，变更 `host/` 或 Core 必须随新版 Gamer 发布；`.gplugin` 只包含 manifest 和 UI。

## 验证

UI：`pnpm --dir plugins/gamer-live/ui test`。

宿主：`cargo test --locked --no-default-features --manifest-path server/Cargo.toml extensions::live`。

真实 FFmpeg 合成素材验证：`cargo test --locked --no-default-features --manifest-path server/Cargo.toml media::output::tests -- --include-ignored`。此测试不连接设备、不公开直播。真实账号权限、直播姬 GUI 导入效果仍需在实际使用环境验证。

协议依据：[B 站开放平台文档](https://open.bilibili.com/doc)、[长连信息](https://open.bilibili.com/doc/4/da2b13dc-7f7a-0025-be11-0b677e793baa)、[长连协议](https://open.bilibili.com/doc/4/5cac94fe-57f9-06db-7515-523d81c44f85)、[签名规范](https://open.bilibili.com/doc/4/8673959e-f7bb-56e6-6e68-d225f971b81b)、[OpenLive 官方示例](https://github.com/bilibili-openplatform/OpenLive_CSharpDemo)。
