const SECRET_KEYS = /^(api_key|authorization|cookie|password|secret|token|access_token|refresh_token|mcp_token|encrypted_content)$/i

// Snapshots are already sanitized by the host; keep a small display guard without
// dropping public request options such as reasoning, thinking or max_tokens.
export function publicRequest(value) {
  if (Array.isArray(value)) return value.map(publicRequest)
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, child]) => [key,
      SECRET_KEYS.test(key) && (child === null || typeof child !== 'object' || Array.isArray(child)) ? '[已脱敏]' : publicRequest(child)]))
  }
  if (typeof value === 'string' && /^data:image\//i.test(value)) return '[图片字节已省略]'
  return value
}

export function requestItems(snapshot) {
  const body = publicRequest(snapshot?.request_body || {})
  const items = Array.isArray(body.input) ? body.input : Array.isArray(body.messages) ? body.messages : typeof body.input === 'string' ? [{role:'user',content:body.input}] : []
  const values = typeof body.instructions === 'string' ? [{ role: 'system', content: body.instructions, request_field: 'instructions' }, ...items] : items
  return values.map((item, index) => ({ index: index + 1, item, role: item.role || item.type || '上下文', text: typeof item.content === 'string' ? item.content : JSON.stringify(item, null, 2) }))
}

export function requestTools(snapshot) {
  const tools = snapshot?.request_body?.tools
  return Array.isArray(tools) ? tools.map(tool => ({ name: tool.name || tool.function?.name || tool.type || '未命名工具', value: publicRequest(tool) })) : []
}

export const SCOPE_LABELS = { chat: '对话', game: '游玩', import: '记忆整理' }
export function toolAccess(snapshot, scope) {
  const tools = requestTools(snapshot)
  if (tools.some(tool => /^(gameplay_(start|status|pause|stop|resume|handoff)|agent_continue)$/.test(tool.name))) return '本轮提供 Agent 游玩编排工具，可根据消息查询记忆、检查设备状态并安排游玩。设备操作仍须通过真实会话和暂停屏障；查询或修改记忆不会恢复暂停。'
  if (tools.some(tool => /^(screen_capture|input_|app_|session_finish)/.test(tool.name))) return '本轮已提供设备观察与操作工具，实际调用仍由设备会话权限和暂停屏障校验。'
  if (scope === 'chat') return '本轮仅提供攻略与记忆等工具，未授予设备操作。是否安排游玩由 Agent 根据消息与设备状态判断；请查看实际工具目录。'
  if (scope === 'import') return '后台记忆整理不提供设备操作工具。'
  return '本轮未提供设备操作工具；请查看实际工具目录和设备会话状态。'
}
