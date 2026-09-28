import { afterEach, expect, it, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import { badges, eventText, giftDetails, safeImage } from './live-log-format'
const call = vi.hoisted(() => vi.fn())
vi.mock('../../../web/src/api', () => ({ api: { callExtension: call } }))
import LiveLogs from './LiveLogs.vue'
let wrapper
afterEach(() => { wrapper?.unmount(); vi.useRealTimers() })
const row = (seq, state = 'waiting') => ({ seq, at: Date.now(), rule: '日常', result: '已入队', event: { kind: 'message', actor: { name: '观众', medal_name: '粉丝牌', medal_level: 12, medal_wearing: true }, payload: { text: '日常' } }, item: { number: seq, state, event: {}, created_at: Date.now() } })
it('缺失字段不伪造身份，未佩戴粉丝牌只在详情显示，礼物不将连击乘入数量', () => {
  expect(badges({})).toEqual([])
  expect(badges({ medal_name: '牌', medal_level: 10, medal_wearing: false })).toEqual([])
  expect(badges({ medal_name: '牌', medal_level: 10, medal_wearing: false }, true)).toEqual(['牌 Lv.10（未佩戴）'])
  expect(eventText({ kind: 'gift', payload: { gift_name: '花', count: 2, combo: { combo_count: 20 } } })).toBe('花 × 2')
  expect(giftDetails({ price: 1000, actual_price: 500 })).toEqual(['单件标价 ¥1', '实际价值 ¥0.5'])
  expect(safeImage('javascript:alert(1)')).toBe('')
})
it('固定阅读时仍更新执行状态，恢复后合并新消息，加载历史使用稳定游标', async () => {
  vi.useFakeTimers()
  call.mockReset().mockResolvedValueOnce({ rows: [row(2)], updates: [], next: 2, has_more: true })
  wrapper = mount(LiveLogs); await flushPromises()
  await wrapper.get('.record').trigger('click')
  await wrapper.findAll('button').find(b => b.text() === '暂停列表滚动').trigger('click')
  call.mockResolvedValueOnce({ rows: [row(3), row(2, 'running')], updates: [row(2, 'running')], next: 2, has_more: true })
  await vi.advanceTimersByTimeAsync(1500); await flushPromises()
  expect(wrapper.findAll('.record')).toHaveLength(1)
  expect(wrapper.text()).toContain('执行中')
  expect(wrapper.text()).toContain('新记录 1+')
  call.mockResolvedValueOnce({ rows: [row(3), row(2, 'running')], updates: [], next: 2, has_more: true })
  await wrapper.findAll('button').find(b => b.text().startsWith('继续更新')).trigger('click'); await flushPromises()
  expect(wrapper.findAll('.record')).toHaveLength(2)
  call.mockResolvedValueOnce({ rows: [row(1)], updates: [], next: 1, has_more: false })
  await wrapper.findAll('button').find(b => b.text() === '加载更早记录').trigger('click'); await flushPromises()
  expect(call.mock.calls.at(-1)[2].before).toBe(2)
  expect(wrapper.findAll('.record')).toHaveLength(3)
  expect(wrapper.findAll('.detail')).toHaveLength(1)
})
