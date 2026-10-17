# 直播助手

提供 Android 音视频和浏览器画面输出、文件夹视频自动直播、直播互动接入，以及弹幕／礼物触发函数或自动化的串行队列。自动直播需要包含此原生实现的 Gamer 本体；插件版本 0.2.14。

### 用户次数、本人确认与主动弹幕

用户管理页可按用户名或完整 open_id 搜索，修改余额或设为不限次数。新观众默认 1 次，修改默认值仅影响后来出现的用户。规则 `cost` 默认为 1，须为正整数；入队时原子预占，余额减预占不足就拒绝，不切设备、不开始扫码。已入队任务的消耗和免扣状态按入队时固定，切换不限次数影响后续入队。模拟账号单独隔离。账户按平台、接入应用、直播间和 open_id 绑定，改昵称或重连不会得到新的次数；数据保存在本机 `extension-data/gamer-live/queue.json`，不会随历史日志清理或配置包导出。

直播日常规则应声明 `completion: {entrypoint: "hsr/云游戏日常.yaml", checkpoint_id: hsr_daily_final_reward}`。只认同一主运行中该脚本实际核验最后领奖的 checkpoint 日志，成功才扣除预占，失败退回。领奖已成功后收尾异常仍算成功；核验证据读取失败则保留预占并提示核对，避免误退或误扣。

规则可配置 `confirmation: {timeout_secs: 300, cancel_cleanup: {entrypoint: "hsr/直播云游戏收尾.yaml", args: {reset_before_logout: false}, timeout_secs: 60}}`。脚本在检测到“进入游戏”后调用 `wait_live_confirmation: {timeout: 5m}`，只让当前任务发起人发送的“确认”放行；“取消”或截止后退出账号。确认和取消不收费，提前、他人、迟到或重复消息不会匹配普通任务规则。截止时间从首次开始等待计时，重复 begin/poll 或重连不延长。普通结束收尾仍重置输入，确认取消的收尾直接执行更多、退出登录和确认。

设置中的主动弹幕用独立的发言账号：填写目标直播间以及自己的 `SESSDATA`、`bili_jct` 单项值，启用后可手动提交和自动提醒。留空沿用已保存值，清除发言账号会停用。Windows 使用当前系统账号的 DPAPI；Unix 使用属主专用目录/文件权限，文件内容未加密。凭据不回显、不进入配置包或任务日志。状态刷新可查看实际发送结果，保存配置不代表已验证登录成功。

自动提醒支持 `{viewer}`、`{number}`、`{position}`、`{cost}`、`{remaining}`、`{available}`；留空关闭单项。消息固定带 `【Gamer】` 前缀，以免自己收到的提醒再触发规则。手动正文最多 33 个字符，自动提醒渲染后限制总长 40；这是本地上限，平台账号实际字数/频率限制和鉴权仍需实连验证。后台最多等待 32 条，间隔至少 3 秒，过期消息丢弃，网络未知结果不自动重发。发送失败独立记录、不改变排队或收费结果，模拟测试不发送提醒。

观众反馈网页沿用原本体端口和地址，展示用户名、粉丝牌等级、完整 open_id、可用/预占次数及本人确认倒计时，无粉丝牌时显示对应空态，不另弹窗口。

## 配合直播姬

1. 安装包含 `gamer-live` 原生实现的 Gamer 本体，再安装并启用本插件。
2. 在「直播助手」选择设备，保留「直播姬多媒体素材」，点击「开始输出」。
3. 复制本机素材地址，在同一台电脑的直播姬添加「多媒体」素材，粘贴地址。
4. 麦克风、场景、直播分区及正式开播由直播姬管理。

地址指向 Gamer **服务端所在电脑**的 `127.0.0.1`，复用本体 HTTP 端口（默认 8443），不再启动额外监听端口。素材地址在直播设置中始终可复制；停止再启动、切换设备或重启服务后继续沿用，不适用于远程直播姬。本地素材使用 HLS，通常有数秒缓冲；首版不提供低延迟预览协议。设备静止时保持最近画面，设备不提供声音时输出静音。关闭插件面板不停止输出；「停止输出」、停用插件、卸载插件或退出服务端会清理进程和临时素材。

需要直接推流时，可选择「手动 RTMP / RTMPS」，填写完整服务器地址和推流码组成的 URL。点击开始即发送设备音视频，插件不采集麦克风，不自动获取平台推流码，也不代替平台的开播授权。目标地址仅在进程内使用，状态响应不返回它。

要求 FFmpeg 包含 H.264 解码、libx264、Opus 解码、AAC 编码、Matroska、HLS 和 FLV 支持。视频采用持续解码与定时编码，存在 CPU 开销；负载较高时降低帧率、设备分辨率或码率。一个插件实例同时支持一路音视频输出和一个互动账号。

## 脚本切换输出设备

启用 gamer-yaml 0.3.15+ 后，可调用 `switch_live_device: {}` 切换到当前脚本运行目标，或 `switch_live_device: {device_id: browser-example, timeout: 30s}` 指定目标。仅切换已经开始的输出，保留原输出方式、素材地址、画幅、帧率和码率；新画面准备失败保留原设备，不自动开播。不同尺寸画面等比例居中。浏览器输出复用原始画面生产者，可与预览和录制并行，不占用自动化运行槽，当前声音为静音。Android 保留游戏声音。

## 自动直播：文件夹视频与弹幕互动

在「直播设置」将「直播模式」选为「自动直播 · 视频与弹幕互动」。这是一条由直播助手管理的后台流程，不需要创建常驻 YAML，也不占用设备的脚本运行槽。

1. 保存互动执行设备和配置包，在「互动规则」中设置弹幕与已有函数／自动化的对应关系并开启规则。所有规则共用当前绑定目标和同一条 FIFO。
2. 填写 **Gamer 服务端所在电脑**的视频文件夹路径，点击「查看视频列表」。只扫描该目录，不读取子目录，不复制或导入媒体库；按文件名排序，启动时固定列表，最多 1000 个视频。
3. 选择直播姬本机素材或手动 RTMP / RTMPS 输出，点击「启动自动直播」。互动账号仍使用现有连接入口；正式开播、麦克风和场景仍由直播姬或直播平台管理。
4. 空闲时循环播放视频。收到匹配消息后入队，设备空闲时先切到实时画面，再执行队首脚本。执行期间持续接收弹幕，依次排队；前项真正结束并释放运行槽后才处理后项。
5. 前项成功且队列仍有等待项时保持实时画面。单项执行失败只记录失败状态与错误，等待运行槽释放后先恢复视频，再继续尝试后续弹幕；队列清空后从上次实际输出的视频帧位置续播。切换沿用同一编码器、输出地址、画幅、帧率和码率。浏览器实时画面仍为静音；文件视频可包含原音频。

支持 MP4、MKV、MOV、WebM、AVI、M4V、TS、FLV、WMV、MPEG 和 MPG（实际解码能力取决于 FFmpeg）。查看列表只检查目录和扩展名，不承诺每个文件可解码；播放时跳过不可用文件并记录日志，全部不可用时显示错误。

自动模式运行时，画面由流程统一控制，手动输出、设备切换和脚本 `switch_live_device` 会被拒绝；规则、日志及队列管理仍可使用。设备上的手动／定时运行按原设备互斥处理，自动直播等待它们结束，不抢占。单项执行失败不会暂停队列，错误保留在触发历史中；取消当前互动后仍需核对目标状态并恢复队列。设备、依赖、画面准备或视频恢复失败暂停派发并保留等待项，可在「触发日志」处理问题后恢复。

「停止自动直播」立即停止受理新触发，清空等待项，请求取消本流程当前互动；确认运行与租约收尾后关闭输出。它不会取消设备上其他来源的运行，互动账号连接仍保留。停止后即使规则开关仍开启，也不会继续触发；重新启动自动直播，或显式开始普通设备输出，才恢复对应模式。准备输出期间也可以停止，旧异步操作不会在停止后启动后项。

关闭面板不停止后台流程。服务重启不会自动连接或开播，遗留队列仍按下文核对规则恢复。视频路径与执行设备、配置包保存在本机队列配置中，下次打开会回填；本次播放配置在当前服务进程内保留，推流地址不进入队列、配置包或日志。

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

规则可配置同一配置包内的独立收尾入口 `cleanup: {entrypoint, args, timeout_secs}`（默认不配置，收尾超时默认 180 秒）。主脚本成功或失败后都执行收尾，等待收尾结束并释放设备再恢复视频或处理下项。主脚本结果与收尾结果分别保留；日常完成即成功，后续刷新／退出失败仅显示收尾警告。停止／取消任务不再启动新的收尾操作，已经启动的收尾会被请求取消。重启遇到未结束的收尾需人工核对，不自动重跑。

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

## 观众反馈网页

在「直播设置」打开「观众反馈」开关，即可复制网页地址，在运行 Gamer 服务端的同一台电脑上交给直播姬的网页素材。无需配置布局、字号或显示数量，也不会自动弹出新窗口；可以先点击「预览网页」检查画面。

网页以紧凑纵向队列展示：顶部标明「空闲中／任务中」，置顶只保留当前任务；没有当前任务时保留最近一次完成、失败或取消结果。其下按顺序列出所有等待项，状态文字与颜色共同区分；底部显示主播自定义的提示文字。提示文字在直播助手编辑并保存，支持换行与留空，最多 1000 个字符，已打开网页会自动更新。

开启只提供网页服务和固定 URL，不会弹出独立窗口或打开浏览器；将 URL 填入同机直播姬的浏览器／网页素材即可。设置只保留开关、复制地址和提示文字，不提供预览新窗口入口。

关闭开关立即停止网页读取，不会停止互动或队列；再次打开沿用同一地址，关闭时仍可复制。反馈网页和状态读取复用本体 HTTP 端口，不再开新端口。切换页面或关闭管理面板不会关闭反馈网页，重新进入面板会读回当前开关和地址。提示文字保存在服务端 `extension-data/gamer-live/audience.json`，不进入配置包；服务或插件重启后展示关闭，提示文字保留。网页只读取公开操作名、观众显示名和排队结果，显示更新中断时不会继续声称状态实时。

素材和反馈使用两个独立的本机只读地址，读取密钥保存在 `extension-data/gamer-live/local-links.json`。同一数据目录和端口下，地址在服务重启后也保持一致；文件损坏会报错，不会静默换地址。地址形式分别为 `/local/extensions/gamer-live/<素材读取密钥>/stream/index.m3u8` 和 `/local/extensions/gamer-live/<反馈读取密钥>/audience/`，实际完整地址在直播助手复制。无需 Gamer 管理登录，真实回环来源、Host／Origin、读取密钥和当前启用状态共同限制读取范围，不能通过该入口调用管理操作。

## 动作与扩展边界

统一通过 `POST /api/extensions/gamer-live/call` 调用，插件必须处于 Running，能力清单来自 `/api/extensions/gamer-live/capabilities`。

| action | values | 权限 |
|---|---|---|
| `live.status` | `{}`，含 `local_sources:{stream_url,audience_url}` 两个固定本机地址 | 无额外权限，仍需认证和 Running |
| `stream.start` | `{device_id, mode:"local"或"rtmp", push_url?, audio?, fps?, bitrate_kbps?}` | `media.stream` |
| `stream.stop` | `{}` | `media.stream` |
| `stream.switch` | `{device_id}`；先准备新画面，再切换，保留地址、画幅和编码参数 | `media.stream` |
| `automatic.preview` | `{video_folder}` → `{folder,files}`，仅查看本地目录文件列表 | `media.stream` |
| `automatic.start` | `{video_folder,device_id,package_id,mode?,push_url?,audio?,fps?,bitrate_kbps?}`，启动专用流程 | `media.stream` + `run.submit` |
| `automatic.status` | `{}` → 状态、视频文件夹、执行目标、输出状态与队列；`live.status` 同时附带 `automatic` | 无额外权限 |
| `automatic.stop` | `{}`，停止受理、清等待并等待当前互动取消收尾后停止输出 | `media.stream` + `run.submit` + `run.control` |
| `connection.connect` | `{platform_id:"bilibili", credentials:{mode,access_key,access_secret?,app_id?,identity_code?,access_token?},expected_version?}` | `live.connect` |
| `connection.disconnect` | `{}` | `live.connect` |
| `connection.settings.read` | `{}` → `{version,mode,profiles}`；profiles 只含公开字段及秘密存在标记 | `live.connect` |
| `connection.settings.save` | `{credentials,expected_version}`；秘密留空沿用同模式同账号的已保存值 | `live.connect` |
| `connection.settings.clear` | `{mode,expected_version}`；删除该模式的配置 | `live.connect` |
| `events.read` | `{after:0}`，后续传上次 `next_seq` | `live.connect` |
| `audience.read` | `{}` → `{enabled,url,text,version}`；关闭时仍返回固定地址，可复制 | `live.connect` |
| `audience.save` | `{text,expected_version}`；按版本保存提示文字并实时更新网页 | `live.connect` |
| `audience.open` | `{}`；开启只读反馈网页，已开启时保留同一地址，返回同 read | `live.connect` |
| `audience.close` | `{}`；停止读取，保留固定地址及提示文字，返回同 read | `live.connect` |
| `rules.read` | `{package_id}` → `{schema_version,rules,version}` | `resource.read` |
| `rules.save` | `{package_id,expected_version,ruleset:{schema_version:1,rules}}` | `ui.host` + `run.submit` |
| `rules.toggle` | `{package_id,id,enabled,expected_version}`，只更新指定规则开关 | `ui.host` + `run.submit` |
| `rules.preview` | `{kind:"message"或"gift",payload:{text?,gift_id?,count?}}` | `resource.read` |
| `queue.status` | `{offset?:0,filter?:"failed"}`，历史每页 30 项 | 无额外权限 |
| `queue.configure` | `{device_id,package_id,video_folder?}`，保存本机执行目标与可选的视频文件夹 | `run.submit` |
| `queue.test` | 同 preview，另需唯一 `request_id` | `run.submit` |
| `queue.control` | `{op,ids?:[],request_id?}`；op 为 resume/remove/clear/cancel/stop/resolve/retry/unbind（resume 仅用于异常恢复，stop 用于插件停止生命周期）；retry 需 request_id | `run.submit` + `run.control` |

事件协议 v1：`schema_version, seq, platform_id, connection_id, room_id, event_id, kind, occurred_at, received_at, actor, payload, platform_data`。统一种类含弹幕、礼物、醒目留言、舰长、点赞、进入和房间状态；原始平台字段保留在 `platform_data`。未知种类返回 `platform.event`，缺失身份返回 null，不猜测 UID；不把礼物价格当作收益。事件读取是有界的进程内观察缓存，不是可靠任务队列：最近 500 条，每次最多 100 条，单事件上限 64 KiB，同连接消息 ID 去重；落后于缓存时 `gap=true`。此观察缓存不保存完整消息历史、不发送弹幕；触发摘要与运行队列按上文单独保存。

平台适配位于 `host/bilibili.rs`，事件归一化位于 `host/events.rs`，插件生命周期与状态位于 `host/mod.rs`；通用音视频输出归 Core `media/output.rs`。后续平台扩展增加平台适配与字段映射，沿用同一输出机制和事件信封，无须将平台接口写入 Core。本插件是 builtin，变更 `host/` 或 Core 必须随新版 Gamer 发布；`.gplugin` 只包含 manifest 和 UI。

## 验证

UI：`pnpm --dir plugins/gamer-live/ui test`。

宿主：`cargo test --locked --no-default-features --manifest-path server/Cargo.toml extensions::live`。

真实 FFmpeg 合成素材验证：`cargo test --locked --no-default-features --manifest-path server/Cargo.toml media::output::tests -- --include-ignored`。此测试不连接设备、不公开直播。真实账号权限、直播姬 GUI 导入效果仍需在实际使用环境验证。

协议依据：[B 站开放平台文档](https://open.bilibili.com/doc)、[长连信息](https://open.bilibili.com/doc/4/da2b13dc-7f7a-0025-be11-0b677e793baa)、[长连协议](https://open.bilibili.com/doc/4/5cac94fe-57f9-06db-7515-523d81c44f85)、[签名规范](https://open.bilibili.com/doc/4/8673959e-f7bb-56e6-6e68-d225f971b81b)、[OpenLive 官方示例](https://github.com/bilibili-openplatform/OpenLive_CSharpDemo)。
