import { expect, it } from 'vitest'
import { chatTimeline, eventDetails, pauseGuidance, safeImage, tokenUsage, usageValue } from './ai-format'
it('token usage缺失和null不会误报零', () => {
  expect(usageValue({ total_tokens: null }, ['total_tokens'])).toBe('未知')
  expect(usageValue({}, ['total_tokens'])).toBe('未知')
  expect(usageValue({ total_tokens: 0 }, ['total_tokens'])).toBe('0')
})
it('图片仅接受inline标准图片数据，不接受远程URL或脚本', () => {
  expect(safeImage('https://example.com/game.png')).toBe('')
  expect(safeImage('data:text/html;base64,AA==')).toBe('')
  expect(safeImage('data:image/png;base64,AA==')).toBe('data:image/png;base64,AA==')
})
it('详情对嵌套secret/token作脱敏，保留无敏感操作数据', () => {
  const value = eventDetails({ x: 42, result: { token: 'a', api_key: 'b', y: 7 }, content: [{ image_data_url: 'c', text: '完成' }] })
  expect(value).toContain('42'); expect(value).toContain('完成'); expect(value).toContain('7')
  expect(value).not.toContain('api_key'); expect(value).not.toContain('token'); expect(value).not.toContain('image_data_url')
})

it('未知总token显示已知累计下限，完整报告显示准确总量', () => {
  expect(tokenUsage({ total_tokens: null, known_tokens: 105396, has_unknown_tokens: true })).toBe('至少 105,396')
  expect(tokenUsage({ total_tokens: 120000, known_tokens: 105396 })).toBe('120,000')
})

it('用户消息按提交时间排序，不因稍后发布事件而落在回复之后', () => {
  const timeline = chatTimeline({ state: 'running', messages: [
    { id: 'm', role: 'user', text: '先观察再点击', at: '2026-10-03T12:00:00Z' },
  ], events: [
    { seq: 1, kind: 'assistant', message: '我先核对画面', at: '2026-10-03T12:00:01Z', data: {} },
    { seq: 2, kind: 'user', message: '稍后发布的消息事件', at: '2026-10-03T12:00:02Z', data: { message_id: 'm' } },
  ] })
  expect(timeline.map(item => item.kind)).toEqual(['user', 'assistant'])
  expect(timeline[0].at).toBe('2026-10-03T12:00:00Z')
})

it('统一tool事件的开始与失败结果合并，同call_id跨generation保留独立卡片', () => {
  const timeline = chatTimeline({ state: 'running', events: [
    { seq: 1, kind: 'tool', message: '正在执行 input_tap', data: { phase: 'start', generation: 1, call_id: 'reused', tool: 'input_tap', arguments: { x: 4 } } },
    { seq: 2, kind: 'tool', message: '工具 input_tap 执行失败', data: { phase: 'result', generation: 1, call_id: 'reused', tool: 'input_tap', ok: false, result: { error: '旧截图' } } },
    { seq: 3, kind: 'tool', message: '正在执行 input_tap', data: { phase: 'start', generation: 2, call_id: 'reused', tool: 'input_tap', arguments: { x: 7 } } },
  ] })
  expect(timeline).toHaveLength(2)
  expect(timeline[0]).toMatchObject({ key: 'event:1', status: 'error', label: '点击', data: { arguments: { x: 4 }, result: { error: '旧截图' } } })
  expect(timeline[1]).toMatchObject({ key: 'event:3', status: 'running', data: { generation: 2, arguments: { x: 7 } } })
})

it('暂停卡使用自身详细原因与建议，历史暂停不套用当前原因', () => {
  const older = { title: '连接失败', detail: '连续请求失败三次', suggestion: '检查模型连接后继续', at: '2026-10-03T11:00:00Z' }
  const current = { title: 'Token 使用达到预算', detail: '已知累计至少 105396，预算 100000', suggestion: '提高 token 上限或改为0后继续', at: '2026-10-03T12:00:00Z' }
  const timeline = chatTimeline({ state: 'paused', generation: 3, reason: current.title, pause_reason: current, events: [
    { seq: 1, kind: 'state', message: 'AI 已暂停：连接失败', data: { state: 'paused', pause_reason: older } },
    { seq: 2, kind: 'state', message: 'AI 已暂停：Token 使用达到预算', data: { state: 'paused', pause_reason: current } },
  ] })
  expect(timeline).toHaveLength(2)
  expect(timeline[0].message).toBe(older.detail)
  expect(timeline[0].current).toBeUndefined()
  expect(pauseGuidance({ pause_reason: timeline[0].pause })).toBe(older.suggestion)
  expect(timeline[1]).toMatchObject({ current: true, message: current.detail, pause: current })
})

it('用户历史不因事件截断丢失，去重user事件且只显示公开摘要', () => {
  const timeline = chatTimeline({ state: 'running', messages: [
    { id: 'old', role: 'user', text: '初始目标', at: '2026-10-03T11:00:00Z' },
    { id: 'new', role: 'user', text: '新的指令', at: '2026-10-03T12:00:00Z' },
  ], events: [
    { seq: 1, kind: 'user', message: '事件摘要', at: '2026-10-03T12:00:00Z', data: { message_id: 'new' } },
    { seq: 2, kind: 'user', message: '重复事件', data: { message_id: 'new' } },
    { seq: 3, kind: 'decision', message: '先观察确认按钮状态', data: {} },
    { seq: 4, kind: 'reasoning', message: '不公开的推理过程', data: {} },
  ] })
  expect(timeline.filter(item => item.kind === 'user').map(item => item.message)).toEqual(['初始目标', '新的指令'])
  expect(timeline.some(item => item.message === '先观察确认按钮状态')).toBe(true)
  expect(timeline.some(item => item.message === '不公开的推理过程')).toBe(false)
  expect(eventDetails({ public_summary: '可公开', reasoning: '不可公开', nested: { private_thought: '不可公开' } })).not.toContain('不可公开')
})
