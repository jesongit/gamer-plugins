export const ACTIVE_STATES = new Set(['starting', 'running', 'pausing', 'paused', 'resuming', 'stopping'])
export const STATE_LABELS = {
  starting: '准备中', running: 'AI 控制中', pausing: '正在暂停', paused: '已暂停 · 可人工操作',
  resuming: '正在恢复', stopping: '正在停止', finished: '已结束', failed: '失败', cancelled: '已停止', budget: '预算已用尽',
}
export function isActive(session) { return !!session && ACTIVE_STATES.has(session.state) }
export function stateLabel(state) { return STATE_LABELS[state] || state || '未开始' }
export function usageValue(usage, names) {
  for (const name of names) {
    const value = usage?.[name]
    if (value != null && Number.isFinite(Number(value))) return Number(value).toLocaleString('zh-CN')
  }
  return '未知'
}
export function budgetValue(limits, key) {
  return limits?.[key] === 0 ? '不限' : usageValue(limits, [key])
}
export function safeImage(value) {
  return typeof value === 'string' && /^data:image\/(png|jpe?g|webp);base64,[a-zA-Z0-9+/=]+$/.test(value) ? value : ''
}
export function eventImage(event) { return safeImage(event?.data?.image_data_url) }
export function eventDetails(data) {
  if (!data || typeof data !== 'object') return ''
  function redact(value) {
    if (Array.isArray(value)) return value.map(redact)
    if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value)
      .filter(([key, child]) => !(value.type === 'image' && key === 'data')
        && !/image_data_url|image_url|api_key|authorization|cookie|password|secret|base64|reasoning|chain.of.thought|private.thought/i.test(key)
        && (!/token/i.test(key) || typeof child === 'number'))
      .map(([key, child]) => [key, redact(child)]))
    if (typeof value === 'string' && value.startsWith('data:image/')) return '[图片]'
    return value
  }
  const safe = redact(data)
  if (!Object.keys(safe).length) return ''
  return JSON.stringify(safe, null, 2)
}
export function displayTime(value) {
  if (value == null) return ''
  const date = typeof value === 'number' ? new Date(value < 1e12 ? value * 1000 : value) : new Date(value)
  return Number.isFinite(date.getTime()) ? date.toLocaleTimeString('zh-CN', { hour12: false }) : String(value)
}
export const PROTOCOLS = {
  responses: { label: 'Responses', baseUrl: 'https://open.bigmodel.cn/api/v1' },
  chat_completions: { label: 'Chat Completions', baseUrl: 'https://open.bigmodel.cn/api/paas/v4' },
}

export const TOOL_LABELS = {
  target_list: '查看目标', context_get: '读取上下文', session_status: '查询会话', screen_capture: '观察画面',
  input_tap: '点击', input_press: '长按', input_swipe: '滑动', input_key: '按键', input_text: '输入文字',
  app_launch: '启动应用', app_stop: '停止应用', wait: '等待', session_finish: '结束会话',
  agent_continue:'继续对话',gameplay_status:'查询游玩状态',gameplay_start:'安排游玩',gameplay_resume:'安排继续游玩',
  gameplay_pause:'暂停游玩',gameplay_stop:'停止游玩',gameplay_handoff:'执行游玩计划',
  memory_search:'检索记忆',memory_get:'阅读记忆',memory_list:'列出记忆',memory_history:'读取修订历史',
  memory_create:'保存新记忆',memory_update:'修订记忆',memory_disable:'停用记忆',memory_delete:'删除记忆',memory_restore:'恢复记忆',memory_merge:'合并记忆',
  memory_import:'暂存攻略',memory_import_jobs:'查询导入进度',memory_source_get:'阅读攻略来源',
  memory_source_update:'修订攻略来源',memory_source_delete:'删除攻略来源',memory_import_pause:'暂停合并',memory_import_resume:'继续合并',memory_import_cancel:'取消合并',
  web_search:'联网检索',web_read:'读取网页',memory_index_status:'查询索引',memory_index_rebuild:'重建索引',
}
export function pauseGuidance(session) {
  if (session?.pause_reason?.suggestion) return session.pause_reason.suggestion
  const reason = session?.reason || ''
  if (/预算|上限|token/i.test(reason)) return '请打开运行预算，提高已耗尽的上限或设为 0（无上限），再明确继续。调整预算会保留本会话的累计用量。'
  if (/租约|令牌|MCP/.test(reason)) return '请检查外部客户端连接和令牌，再由你点击“继续 AI”；重新连接不会自行恢复。'
  if (/目标|绑定|断开|重建/.test(reason)) return '请检查目标连接。目标身份改变时，先停止旧会话，再开始新会话。'
  if (/失败|模型|请求|观察/.test(reason)) return '请检查连接和当前画面，可补充指令后明确继续；重复失败时先测试模型连接。'
  return '现在可以人工操作，也可以补充指令。需要 AI 继续时，请明确发送并继续或点击“继续 AI”。'
}
export function tokenUsage(usage) {
  if (usage?.total_tokens != null) return usageValue(usage, ['total_tokens'])
  if (Number(usage?.known_tokens) > 0) return `至少 ${usageValue(usage, ['known_tokens'])}`
  return '未知'
}
export function chatTimeline(session) {
  if (!session) return []
  const messages = session.messages || []
  const known = new Map(messages.map(message => [message.id, message]))
  const used = new Set(), items = [], calls = new Map()
  const publicKinds = new Set(['user', 'assistant', 'decision', 'tool', 'tool_start', 'tool_result', 'observation', 'capture', 'state', 'error', 'model', 'progress'])
  for (const event of (session.events || []).slice(-200)) {
    if (!publicKinds.has(event.kind)) continue
    const messageId = event.data?.message_id
    if (event.kind === 'user' && messageId && used.has(messageId)) continue
    if (messageId) used.add(messageId)
    const stored = known.get(messageId)
    const entry = { ...event, key: messageId ? `message:${messageId}` : `event:${event.seq}`, message: stored?.text ?? event.message, at: stored?.at || event.at }
    const callId = event.data?.call_id || event.data?.operation_id
    const callKey = callId ? `${event.data?.generation ?? event.generation ?? 'legacy'}:${callId}` : ''
    if (['tool_start', 'tool_result', 'tool'].includes(event.kind)) {
      entry.kind = 'tool'
      entry.tool = event.data?.tool || event.data?.name || ''
      entry.label = TOOL_LABELS[entry.tool] || entry.tool || '工具操作'
      entry.status = event.data?.phase === 'start' || event.kind === 'tool_start' ? 'running'
        : event.data?.ok === false || event.data?.is_error || event.data?.result?.isError ? 'error' : event.data?.status || 'success'
      if (callKey && calls.has(callKey)) {
        const previous = calls.get(callKey)
        Object.assign(previous, entry, { key: previous.key, data: { ...previous.data, ...entry.data } })
        continue
      }
      if (callKey) calls.set(callKey, entry)
    }
    if (event.kind === 'state' && (event.data?.state === 'paused' || event.data?.manual_allowed === true)) {
      entry.kind = 'pause'; entry.pause = event.data?.pause_reason
      entry.message = entry.pause?.detail || entry.message
    }
    items.push(entry)
  }
  for (const message of messages) {
    if (message.role !== 'user' || used.has(message.id)) continue
    items.push({ key: `message:${message.id}`, kind: 'user', message: message.text, at: message.at, data: {} })
  }
  if (!messages.length && session.goal && !items.some(item => item.kind === 'user' && item.message === session.goal)) {
    items.unshift({ key: `goal:${session.session_id}`, kind: 'user', message: session.goal, data: {} })
  }
  items.sort((a, b) => {
    const left = Date.parse(a.at), right = Date.parse(b.at)
    if (Number.isFinite(left) && Number.isFinite(right)) return left - right
    return Number.isFinite(left) ? -1 : Number.isFinite(right) ? 1 : 0
  })
  if (session.state === 'paused') {
    const reason = session.pause_reason?.detail || session.reason || '会话已暂停'
    const existing = items.findLast(item => item.kind === 'pause')
    if (existing && (existing.message === reason || existing.message === session.reason || existing.pause?.at && existing.pause.at === session.pause_reason?.at)) { existing.pause = session.pause_reason; existing.current = true }
    else items.push({ key: `pause:${session.generation}`, kind: 'pause', message: reason, pause: session.pause_reason, current: true, data: {} })
  }
  if (session.state === 'finished' && session.reason && !items.some(item => item.kind === 'state' && item.message?.includes(session.reason))) items.push({ key: `end:${session.session_id}`, kind: 'state', message: `会话结束：${session.reason}`, data: {} })
  return items
}
