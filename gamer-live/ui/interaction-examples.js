export const examples = [
  { name: '弹幕跳跃', kind: 'message', value: '跳', hint: '绑定跳跃函数，次数可填 1' },
  { name: '弹幕放技能', kind: 'message', value: '放技能', hint: '绑定技能函数，选择要释放的技能' },
  { name: '弹幕换角色', kind: 'message', value: '换人', hint: '绑定切换角色函数，填写角色位置' },
  { name: '弹幕开始挑战', kind: 'message', value: '开始挑战', hint: '绑定挑战自动化，填写难度等参数' },
  { name: '礼物放技能', kind: 'gift', value: '', hint: '填写礼物 ID，绑定技能函数；次数可选择礼物数量' },
  { name: '礼物触发挑战', kind: 'gift', value: '', hint: '填写礼物 ID，绑定挑战自动化；发起人可选择观众名字' },
]
export const fields = [
  ['text', '弹幕内容'], ['gift_id', '礼物 ID'], ['gift_name', '礼物名称'],
  ['count', '礼物数量'], ['actor_name', '观众名字'], ['actor_id', '观众 ID'],
]
export function newRule(example = {}) {
  return { id: crypto.randomUUID(), name: example.name || '新规则', enabled: false, kind: example.kind || 'message', operator: 'equals', value: example.value || '', min_count: 1, entrypoint: '', args: {}, cooldown_secs: 0, timeout_secs: 0 }
}
export function fixedValue(text, type) {
  if (type === 'boolean') return text === 'true'
  if (['integer', 'number'].includes(type)) {
    if (String(text).trim() === '' || !Number.isFinite(Number(text)) || (type === 'integer' && !Number.isSafeInteger(Number(text)))) throw new Error('请输入有效数字')
    return Number(text)
  }
  if (['object', 'array', 'list', 'point', 'any'].includes(type)) return JSON.parse(text)
  return String(text)
}
