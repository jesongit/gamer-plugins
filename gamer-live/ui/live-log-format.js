export const states = { starting: '正在启动', running: '执行中', waiting: '排队中', cancelling: '正在取消', review: '结果待核对', acknowledged: '已核对结束', removed: '已移除', success: '已完成', failed: '执行失败', cancelled: '已取消' }
export const kinds = { message: '弹幕', 'message.mirror': '跨房弹幕', gift: '礼物', super_chat: '醒目留言', membership: '上舰', like: '点赞', enter: '进入直播间', follow: '关注', 'room.started': '开播', 'room.ended': '下播', 'message.removed': '留言撤回', 'room.changed': '房间变更', 'room.warning': '平台警告', 'user.blocked': '禁言', 'connection.ended': '推送结束' }
const guards = { 1: '总督', 2: '提督', 3: '舰长' }
export function badges(actor, detail = false) {
  if (!actor) return []
  const badges = []
  if (actor.medal_name && (actor.medal_wearing === true || detail)) badges.push(`${actor.medal_name}${actor.medal_level != null ? ' Lv.' + actor.medal_level : ''}${actor.medal_wearing === false ? '（未佩戴）' : actor.medal_wearing == null ? '（佩戴状态未知）' : ''}`)
  if (guards[actor.guard_level]) badges.push(guards[actor.guard_level])
  if (actor.is_admin === 1 || actor.is_admin === true) badges.push('房管')
  if (detail && actor.glory_level != null) badges.push(`荣耀 ${actor.glory_level} 级`)
  return badges
}
export function eventText(event) {
  const p = event?.payload || {}
  if (event?.kind === 'gift') return `${p.gift_name || '礼物'}${p.count != null ? ' × ' + p.count : ''}`
  if (event?.kind === 'membership') return `${guards[p.guard_level] || '上舰'} · ${p.unit?.startsWith('*') ? p.unit.slice(1) : `${p.count ?? ''}${p.unit || ''}`}`
  if (event?.kind === 'super_chat') return `${p.rmb != null ? '¥' + p.rmb + ' · ' : ''}${p.text || ''}`
  if (event?.kind === 'like') return `点赞${p.count != null ? ' × ' + p.count : ''}`
  if (event?.kind === 'message.removed') return `撤回留言 ${(p.removed_ids || []).join('、')}`
  return p.text || (p.dm_type === 1 ? '[表情弹幕]' : p.title || kinds[event?.kind] || event?.kind || '互动')
}
export function giftDetails(p = {}) {
  const details = []
  if (p.paid != null) details.push(p.paid ? '付费礼物' : '免费礼物')
  if (p.price != null) details.push(`单件标价 ¥${p.price / 1000}`)
  if (p.actual_price != null) details.push(`实际价值 ¥${p.actual_price / 1000}`)
  if (p.combo?.combo_count != null) details.push(`连击累计 ${p.combo.combo_count}（不重复计算数量）`)
  if (p.blind_gift?.status) details.push(`盲盒礼物 ${p.blind_gift.blind_gift_id ?? ''}`)
  return details
}
export function safeImage(url) { try { const parsed = new URL(url); return ['https:', 'http:'].includes(parsed.protocol) ? parsed.href : '' } catch { return '' } }
