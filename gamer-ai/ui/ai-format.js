export const ACTIVE_STATES = new Set(['starting', 'running', 'pausing', 'paused', 'resuming', 'stopping'])
export const STATE_LABELS = {
  starting: '准备中', running: 'AI 控制中', pausing: '正在暂停', paused: '已暂停 · 可人工操作',
  resuming: '正在恢复', stopping: '正在停止', finished: '已结束', failed: '失败', cancelled: '已停止',
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
export function safeImage(value) {
  return typeof value === 'string' && /^data:image\/(png|jpe?g|webp);base64,[a-zA-Z0-9+/=]+$/.test(value) ? value : ''
}
export function eventImage(event) { return safeImage(event?.data?.image_data_url) }
export function eventDetails(data) {
  if (!data || typeof data !== 'object') return ''
  function redact(value) {
    if (Array.isArray(value)) return value.map(redact)
    if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value)
      .filter(([key]) => !/image_data_url|api_key|authorization|token|password|secret|base64/i.test(key))
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
