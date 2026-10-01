# AI 助手

`gamer-ai` 是通用 builtin 插件，提供模型 API 自动游玩和本机 Streamable HTTP MCP。两种入口共用截图、输入、会话控制和工具执行器。Android 与浏览器目标复用宿主已有能力，没有固定游戏脚本。版本为 `0.1.0`，需要 Gamer `0.2.6` 或更高版本中包含对应 builtin 实现。

## 内置 AI 使用

1. 安装并启用插件，在工作台选择设备和配置包。Android 应用目标来自设备的“应用”配置；浏览器目标需先连接并绑定画面。应用与配置包是两个独立上下文。
2. 在“模型连接”填写 API 基础地址、模型与密钥，点击“保存并测试连接”。默认模型为 `glm-5.3-flash`，默认协议为 Responses。基础地址不要加 `/responses` 或 `/chat/completions`，插件按所选协议拼接路径。
3. 测试结果中的模型连接、图片识别、基于图片的工具调用、工具截图反馈和工具结果回传都通过后，在“自动游玩”填写目标、成功条件与操作限制。
4. 选择“内置 AI · 模型 API”，设置预算，点击“开始自动游玩”。会话显示公开回答、截图及操作记录；操作成功表示输入已注入，实际效果通过随后截图判断。
5. 需要人工操作时点击“暂停 AI”，等待状态变为“已暂停”且投屏提示允许人工操作，再点击、按键或输入文字。完成后点击“继续 AI”；服务端重新取得控制权、递增 `generation` 并观察新画面。
6. 结束时点击“停止会话”。关闭面板或管理页面不会停止服务端会话。

建议先用可随时退出、无需高速反应的场景，目标可写为：“观察当前界面，找到设置入口并打开；成功条件是设置页可见；只使用必要操作，无法确认时停止说明原因。”用户自行选择游戏验证模型延迟、定位偏差及任务完成情况。

### 模型连接与真实验证

| 协议 | 基础地址 | 请求路径 |
| --- | --- | --- |
| Responses（默认） | `https://open.bigmodel.cn/api/v1` | `/responses` |
| Chat Completions | `https://open.bigmodel.cn/api/paas/v4` | `/chat/completions` |

2026-10-02 使用实际测试账户与 `glm-5.3-flash` 验证了两种协议：每种协议 4 次请求、5 个能力检查全部通过；合计 8 次请求、供应商回报 2421 tokens。测试包含连接标记、随机合成 PNG 的主色识别、依据图片生成带随机标记的函数调用，以及工具回传另一张图片后的继续识别。这是模型协议验证，不代表特定游戏已通过验收。

连接测试使用非游戏图片与无设备副作用的测试工具，不操作当前设备。若测试不通过，界面展示具体检查结果。可以显式选择 Chat Completions，再点击“填入此地址”、保存并重新测试；插件不会在游玩中静默切换地址或协议。保存配置不会更改已运行代次的连接，下一次开始或恢复使用已保存配置。

API 密钥只保存在宿主 `extension-data/gamer-ai/private/` 私密配置中。公共响应仅返回 `has_key`，密钥输入留空沿用已保存值，输入新值可更新；密钥不进入源码、日志或 Package 导出。发送给模型的内容包括用户目标、必要历史、目标截图和工具结果。每次更换配置后重新测试。

### 控制权与预算

| 状态 | 人工输入 | 操作 |
| --- | --- | --- |
| 开始中、运行中、恢复中 | 锁定 | 可请求暂停或停止 |
| 暂停中 | 锁定 | 等待已入场动作结束、按键/触点释放 |
| 已暂停 | 允许 | 可人工操作，再继续或停止 |
| 停止中 | 清理完成前锁定 | 等待结束 |
| 已结束 | 允许 | 可开始新的会话 |

观看、音频设置及暂停/停止仍可使用，运行模型连接测试也不会阻塞会话暂停/停止。触控、键盘、粘贴、滚轮及应用启停受服务端仲裁，界面禁用只是提示；绕过界面直接调用人工输入接口也不能取得 AI 的控制权。工作台切换设备或配置包不会改写已开始会话的目标。浏览器重新绑定或 Android 应用配置改变时，需要停止旧会话后重新开始。

强制断开、删除设备或修改地址、屏幕模式、分辨率等投屏目标配置，必须先“停止会话”并等待结束，暂停不足以允许这些管理操作；服务端会拒绝操作，不会自动终止 AI。共享 ADB 强制重连还要求所有设备上的活动运行、采集、扩展及录制均已停止。普通“断开连接”只退出本页投屏，不会停止 AI。

默认预算：模型 40 轮、工具 120 次、活动 600 秒、累计 100000 tokens、连续失败 3 次。截图和等待也计入工具次数。暂停保留同一运行槽和累计预算，暂停等待不计入活动时长；YAML、定时或其他自动化需要先停止 AI 会话才能取得该设备运行槽。达到预算后暂停，停止后调整预算开始新会话。供应商未提供 usage 时累计 token 显示“未知”，不会当作零。

插件停用或服务端退出会收尾活动会话。服务端重启不会自动恢复 AI 或重放历史输入。首版不保存目标/预算 profile，不提供持续视频推理、DOM/任意 JavaScript 工具、跨插件自动化或定时 AI 任务。

## 外部 MCP

### 连接与授权

在“外部 MCP”创建独立连接令牌。令牌固定授权一个设备及配置包，可选择“只读观察”或“允许控制”。外部 MCP 不要求配置模型 API 密钥。

- **只读观察**：可以查询上下文及截屏，无需开始 AI 会话。调用 `screen_capture` 时不带 `session_id` 与 `generation`；返回 `generation: 0` 的观察画面，不能将它用于控制输入。
- **允许控制**：先在“自动游玩”选择“外部 AI · MCP 客户端”，明确目标并建立同设备、同配置包的会话，再创建控制令牌。客户端不能通过 MCP 开始、恢复、换目标或绕过用户暂停。

地址为 `/api/extensions/gamer-ai/mcp`。面板按当前页面地址生成示例；客户端必须运行在 Gamer 服务端所在电脑上，直连该服务端的回环地址和实际端口。例如默认端口为 `http://127.0.0.1:8443/api/extensions/gamer-ai/mcp`。管理页面经远程地址或 Vite 开发代理打开时，请改用服务端回环地址；代理带 `Forwarded` 或 `X-Forwarded-For` 会被拒绝。远程管理 Cookie、管理员 token 和非回环客户端均不能替代 MCP Bearer 令牌。

支持 `url` 与 `headers` 的标准客户端可配置：

```json
{
  "mcpServers": {
    "gamer": {
      "url": "http://127.0.0.1:8443/api/extensions/gamer-ai/mcp",
      "headers": { "Authorization": "Bearer <面板中创建的连接令牌>" }
    }
  }
}
```

客户端自动补充 MCP 标准请求头。手工发送 JSON-RPC POST 时必须包含：

```http
Authorization: Bearer <连接令牌>
Content-Type: application/json
Accept: application/json, text/event-stream
MCP-Protocol-Version: 2025-11-25
```

`MCP-Protocol-Version` 在初始化后使用服务端协商返回的版本，支持 `2025-03-26`、`2025-06-18`、`2025-11-25`。HTTP 客户端自动提供回环 `Host`；若提供 `Origin`，必须与 Host 匹配且为回环来源。传输采用无状态 Streamable HTTP POST：普通请求返回 JSON，通知返回 `202`；不发放或要求 `Mcp-Session-Id`，也无需维持 SSE 连接。

初始化请求示例：

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"my-game-client","version":"1.0"}}}
```

初始化后发送 `notifications/initialized` 通知，再通过 `tools/list` 获取当前令牌和目标能力对应的工具。完整令牌仅在创建响应中显示，列表不返回原文；丢失时创建新令牌并撤销旧令牌。

### 工具目录

完整目录共 13 个工具，所有坐标使用返回截图的 `width`/`height`；服务端负责映射到原画面。目标不支持的工具不会出现在 `tools/list` 中；浏览器目标没有 Android 应用启停工具。

| 工具 | 主要参数 | 内容 |
| --- | --- | --- |
| `target_list` | 无 | 返回令牌授权目标和能力，不枚举其他设备 |
| `context_get` | 无 | 当前设备、运行目标身份及配置包 |
| `session_status` | 无 | 同配置包的当前会话、状态、`generation` 与输入控制状态 |
| `screen_capture` | `max_width?`；控制用图带 `session_id`、`generation` | 最新 PNG、`frame_id`、采集时间、截图与原画面尺寸；默认最大宽度 1280，可设 320–1920 |
| `input_tap` | `frame_id,x,y` | 点击后释放 |
| `input_press` | `frame_id,x,y,duration_ms` | 长按并释放，60–3000 毫秒 |
| `input_swipe` | `frame_id,x1,y1,x2,y2,duration_ms` | 滑动并释放，100–3000 毫秒 |
| `input_key` | `frame_id,key,duration_ms?` | 命名按键，如 `Enter`、`Escape`、`ArrowUp`、`KeyW`、`BACK`；按住不超过 3000 毫秒 |
| `input_text` | `frame_id,text` | 输入最多 2000 字符，事件日志隐藏正文 |
| `app_launch`、`app_stop` | `frame_id` | 启停设备已配置的 Android 应用，无任意包名参数 |
| `wait` | `duration_ms` | 可取消等待，1–5000 毫秒 |
| `session_finish` | `message` | 说明完成或无法继续并结束会话 |

除只读查询与 `screen_capture` 外，控制工具还必须带 `session_id`、`generation`、`operation_id`。`wait` 和 `session_finish` 也要求这些字段，但不要求 `frame_id`。截图返回标准 MCP `content` 的 image block 和文本元数据，同时提供 `structuredContent`，不是仅返回文件路径或 Base64 文本。

### 一次控制的请求顺序

1. 调用 `session_status`，取得状态为 `running` 的 `session_id` 和当前 `generation`。会话为空或处于 `paused` 时，由用户在面板处理。
2. 带这两个字段调用 `screen_capture`，读出最新 `frame_id`、`width`、`height` 并观察图片。
3. 为新操作生成唯一 `operation_id`（建议 UUID，最多 128 字节），连同三个身份字段及所需参数调用输入工具。
4. 同一令牌、同一会话和代次内重试同一操作必须复用 `operation_id` 与完整原参数；服务端返回已记录结果，不重复注入。新操作必须用新 ID；相同 ID 修改工具或参数会被拒绝。JSON-RPC 的 `id` 只关联请求与响应，不能用来代替操作去重 ID。
5. 每次成功输入后重新截图，再决定下一步。暂停/恢复改变 `generation`，旧请求失效；画面身份、尺寸或目标连接改变时，旧 `frame_id` 失效。

调用示例（标识均替换为实际值，坐标须依据截图选取）：

```json
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"session_status","arguments":{}}}
```

```json
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"screen_capture","arguments":{"session_id":"<会话ID>","generation":1,"max_width":1280}}}
```

```json
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"input_tap","arguments":{"session_id":"<会话ID>","generation":1,"operation_id":"<本次操作UUID>","frame_id":"<最近截图ID>","x":100,"y":200}}}
```

工具错误通过 `result.isError: true` 和文本说明返回。收到 `stale_generation` 或 `stale_frame` 后先重新查询/观察，不更换去重 ID 盲目重复旧动作。若结果丢失，先以原参数和 ID 重试获取执行收据，再观察；客户端重启丢失这些标识时不能假设旧操作没有执行。

### 令牌与活动租约

令牌有效期固定 24 小时，可随时撤销。`ttl_seconds` 是控制会话的不活跃租约，不是令牌有效期：允许 30–3600 秒，默认 120 秒。有效的绑定会话工具请求或控制令牌的 MCP `ping` 可续租；只读查询、只读令牌及无效/过时代次操作不会取得控制权。客户端应以低于租约期限的间隔发送 `ping`。

建立或恢复外部会话后有初始 120 秒连接窗口；客户端第一次有效会话工具请求或 `ping` 后，按令牌的 `ttl_seconds` 续租，并以令牌的 24 小时到期时间为上限。测试短租约时，先发送一次有效请求，再停止续租并等待指定时长。`ping` 请求示例：

```json
{"jsonrpc":"2.0","id":5,"method":"ping"}
```

租约到期、控制令牌过期或撤销会暂停关联控制会话。重新连接或创建新令牌后仍需要用户点击“继续 AI”；续租不能解除人工暂停。HTTP 返回或连接关闭不等于客户端失联，离线后通过租约期限收回控制。停止会话会释放运行槽。

## 自测清单

- 保存模型配置并运行连接测试，确认五项通过、协议与地址符合选择；切换协议时重新测试。
- 内置模式开始简单目标，核对截图和记录；运行中尝试人工点击、按键、粘贴及应用启停，应被拒绝。
- 暂停时等待“已暂停”，人工点击应恢复；继续后 AI 使用新截图，旧代次/旧画面的外部请求应失败。停止后可运行其他自动化。
- 暂停时修改投屏参数或尝试强制断开，应被要求先停止；停止完成后才允许这些管理操作。运行连接 probe 期间仍可在“自动游玩”暂停和停止会话。
- 创建只读令牌，不开始 AI 会话，完成初始化、工具列表和 `screen_capture`；确认没有输入工具。
- 建立外部 MCP 会话，创建控制令牌，通过上述顺序截图并执行一次操作。原 ID/参数重试不重复输入，原 ID 改参数被拒绝；操作后必须重新截图。
- 暂停外部会话后继续 `ping`，确认仍保持暂停；恢复后先 `ping` 一次，再停止续租并等待租约期限，应暂停。撤销控制令牌也应暂停，恢复必须由用户操作。
- 对 Android 与浏览器分别检查支持的工具；实际游戏完成率和延迟由用户选择游戏验证。

源码层面的 UI、构建及模型协议检查已经完成；真实设备/浏览器贯通和游戏效果仍以对应验收结果为准，不能从连接测试推断游戏已完成。

## 开发与构建

UI 使用 SDK v1 宿主模块以及 `WORKSPACE_CONTEXT_KEY` 只读快照，动态模块导出 `AiWorkspace`。设备、Android 应用、Package 与插件身份独立；业务代码不进入 Core 壳。

```powershell
# 主仓根目录：插件 UI 与宿主 UI 集成
node tools/check-plugin-sdk.mjs
node tools/build-plugin-ui.mjs gamer-ai
# 插件目录：单独验证 UI
cd plugins/gamer-ai/ui
pnpm install --frozen-lockfile
pnpm test
pnpm build
```

插件仓根目录可运行 `./gamer-ai/build.ps1`，或 `./build.ps1 -Plugin gamer-ai`，输出 `dist/plugins/gamer-ai-0.1.0.gplugin`、目录和校验清单。主仓 `tools/build-plugins.ps1` 也能包装；开发时可显式指定输出目录以免覆盖本地市场目录。

执行体位于 `host/` 并编译进宿主，安装 UI 归档需要匹配的宿主版本。独立包装复用 `sdk/lock.json` 固定快照，无 WASM 占位文件，不依赖 gamer-yaml。修改 host 后必须一起构建/发布 Gamer 本体。
