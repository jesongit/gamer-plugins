import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { flushPromises } from '@vue/test-utils'
import { Window } from 'happy-dom'

const html = readFileSync(resolve(process.cwd(), '../host/audience.html'), 'utf8')
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1]
let page, snapshot, fetchMock
const item = (number, state, extra = {}) => ({ number, state, name: '日常', viewer: '观众' + number, test: false, started_at: Date.now() - 72000, ...extra })
beforeEach(() => {
  vi.useFakeTimers()
  snapshot = { current: null, featured: null, waiting: [], waiting_count: 0, time: Date.now(), paused: false, configured: true, device_busy: false, connection: 'connected', text: '' }
  fetchMock = vi.fn(async () => ({ ok: true, json: async () => structuredClone(snapshot) }))
})
afterEach(async () => { vi.clearAllTimers(); vi.useRealTimers(); await page?.happyDOM.close() })
async function open() {
  page = new Window({ url: 'http://127.0.0.1:8443/local/extensions/gamer-live/read-capability/audience/' })
  page.document.write(html.replace(/<script>[\s\S]*?<\/script>/, ''))
  const context = { document: page.document, location: page.location, sessionStorage: page.sessionStorage, history: page.history, fetch: fetchMock, setTimeout, setInterval, AbortSignal, Date }
  new Function(...Object.keys(context), script)(...Object.values(context))
  await flushPromises()
  return page.document
}
async function update(values) { Object.assign(snapshot, values); await vi.advanceTimersByTimeAsync(1000); await flushPromises() }

it('首行只有当前任务，忽略旧历史，完整保留等待FIFO且不截断', async () => {
  snapshot.current = item(88, 'running')
  snapshot.featured = item(87, 'success')
  snapshot.notices = [item(86, 'failed')]
  snapshot.waiting = Array.from({ length: 12 }, (_, index) => item(89 + index, 'waiting', { position: index + 1 }))
  snapshot.waiting_count = snapshot.waiting.length
  const document = await open()
  expect(document.querySelector('#activity').textContent).toBe('任务中')
  expect(document.querySelectorAll('#featured .task-title')).toHaveLength(1)
  expect(document.querySelector('#featured').textContent).toContain('#88')
  expect(document.querySelector('#featured').textContent).not.toContain('#87')
  expect(document.querySelector('#featured').textContent).toContain('已进行 1分12秒')
  expect(document.querySelectorAll('#waiting li')).toHaveLength(12)
  expect([...document.querySelectorAll('#waiting .ticket')].map(node => node.textContent)).toEqual(Array.from({ length: 12 }, (_, index) => '#' + (89 + index)))
  expect(document.querySelector('#queue-total').textContent).toBe('12 项等待')
  expect(document.querySelector('#toast')).toBeNull()
})
it('完整展示姓名、粉丝牌、open_id与额度，确认倒计时使用服务端时间', async () => {
  const openId = '完整标识ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789'
  snapshot.current = item(88, 'running', { viewer: '名字很长的观众', open_id: openId, medal_name: '星海', medal_level: 16, phase: 'awaiting_confirmation', confirmation: { state: 'awaiting_confirmation', deadline_at: snapshot.time + 300000 }, quota: { remaining: 4, reserved: 2, available: 2, unlimited: false } })
  const document = await open()
  expect(document.querySelector('#featured').dataset.state).toBe('awaiting_confirmation')
  expect(document.querySelector('#featured .medal').textContent).toBe('星海 Lv16')
  expect(document.querySelector('#featured .open-id').textContent).toContain(openId)
  expect(document.querySelector('#featured .quota').textContent).toBe('剩余 4 · 预占 2 · 可用 2')
  expect(document.querySelector('#confirmation-time').textContent).toBe('确认剩余 5:00')
  snapshot.time += 1000
  await vi.advanceTimersByTimeAsync(1000); await flushPromises()
  expect(document.querySelector('#confirmation-time').textContent).toBe('确认剩余 4:59')
  await update({ current: item(89, 'running', { quota: { unlimited: true, reserved: 1 } }) })
  expect(document.querySelector('#featured .quota').textContent).toBe('不限次数 ∞ · 预占 1')
  expect(document.querySelector('#confirmation-time')).toBeNull()
})
it('等待队列身份额度也完整显示，全部外部文字保持纯文本', async () => {
  const injected = '<img src=x onerror=alert(1)>'
  snapshot.waiting = [item(90, 'waiting', { open_id: injected, medal_name: injected, medal_level: 8, quota: { remaining: 1, reserved: 1, available: 0 } })]
  const document = await open()
  expect(document.querySelector('#waiting .open-id').textContent).toContain(injected)
  expect(document.querySelector('#waiting .quota').textContent).toContain('剩余 1')
  expect(document.querySelectorAll('img')).toHaveLength(0)
})

it('任务结束后仅保留一个最近结果，完成/失败/取消各有明确文字状态', async () => {
  snapshot.featured = item(20, 'success')
  const document = await open()
  expect(document.querySelector('#activity').textContent).toBe('空闲中')
  expect(document.querySelector('#featured').dataset.state).toBe('success')
  expect(document.querySelector('.task-status').textContent).toBe('已完成')
  await update({ featured: item(21, 'failed') })
  expect(document.querySelectorAll('#featured .task-title')).toHaveLength(1)
  expect(document.querySelector('#featured').textContent).not.toContain('#20')
  expect(document.querySelector('.task-status').textContent).toBe('失败')
  await update({ featured: item(22, 'cancelled') })
  expect(document.querySelector('.task-status').textContent).toBe('已取消')
  expect(document.querySelector('#empty-waiting').hidden).toBe(false)
})

it('空队列没有假任务，公开名称和多行自定义文字按纯文本展示', async () => {
  const document = await open()
  expect(document.querySelector('#featured').textContent).toContain('触发互动后')
  expect(document.querySelectorAll('#featured .task-title')).toHaveLength(0)
  const unsafe = '<img src=x onerror="alert(1)">\n发送日常即可排队'
  await update({ text: unsafe, featured: item(33, 'success', { name: '<script>bad()</script>', viewer: '<img src=x>' }) })
  expect(document.querySelector('#custom-text').textContent).toBe(unsafe)
  expect(document.querySelector('#custom-text').hidden).toBe(false)
  expect(document.querySelectorAll('img,script')).toHaveLength(0)
  expect(document.querySelector('#featured').textContent).toContain('<script>bad()</script>')
  await update({ text: '' })
  expect(document.querySelector('#custom-text').hidden).toBe(true)
})

it('缺少更新时保留最后队列但标明中断，重新收到状态后恢复', async () => {
  snapshot.current = item(8, 'running')
  const document = await open()
  fetchMock.mockRejectedValue(new Error('offline'))
  await vi.advanceTimersByTimeAsync(6000); await flushPromises()
  expect(document.body.classList.contains('stale')).toBe(true)
  expect(document.querySelector('#activity').textContent).toBe('更新中断')
  expect(document.querySelector('#connection').textContent).toContain('显示上次进度')
  expect(document.querySelector('#featured').textContent).toContain('#8')
  fetchMock.mockImplementation(async () => ({ ok: true, json: async () => ({ ...snapshot, current: null, featured: item(8, 'success') }) }))
  await vi.advanceTimersByTimeAsync(1000); await flushPromises()
  expect(document.body.classList.contains('stale')).toBe(false)
  expect(document.querySelector('#activity').textContent).toBe('空闲中')
})

it('通过固定网页地址的相对state读取，不依赖管理Cookie或Bearer，暂停不改任务结果', async () => {
  snapshot.featured = item(55, 'success'); snapshot.paused = true
  const document = await open()
  expect(page.location.hash).toBe('')
  expect(page.sessionStorage.getItem('read-token')).toBeNull()
  expect(fetchMock).toHaveBeenCalledWith('state', expect.objectContaining({ credentials: 'omit', cache: 'no-store' }))
  expect(fetchMock.mock.calls[0][1].headers).toBeUndefined()
  expect(new URL(fetchMock.mock.calls[0][0], page.location.href).pathname).toBe('/local/extensions/gamer-live/read-capability/audience/state')
  expect(document.querySelector('#connection').textContent).toContain('队列已暂停')
  expect(document.querySelector('.task-status').textContent).toBe('已完成')
})

it('设备正被其他任务占用时显示任务中，不伪装成空闲或添加虚构队列项', async () => {
  snapshot.device_busy = true
  const document = await open()
  expect(document.querySelector('#activity').textContent).toBe('任务中')
  expect(document.querySelector('#connection').textContent).toBe('等待当前操作结束')
  expect(document.querySelectorAll('#featured .task-title')).toHaveLength(0)
  expect(document.querySelectorAll('#waiting li')).toHaveLength(0)
})

it('轮询不重建未变化队列，队列变化仍保留滚动位置', async () => {
  snapshot.waiting = Array.from({ length: 20 }, (_, index) => item(index + 1, 'waiting', { position: index + 1 }))
  snapshot.waiting_count = 20
  const document = await open()
  const list = document.querySelector('#waiting'), first = list.firstElementChild
  list.scrollTop = 96
  await update({ time: Date.now() + 1000 })
  expect(list.firstElementChild).toBe(first)
  expect(list.scrollTop).toBe(96)
  await update({ waiting: [...snapshot.waiting, item(21, 'waiting', { position: 21 })], waiting_count: 21 })
  expect(list.children).toHaveLength(21)
  expect(list.scrollTop).toBe(96)
  expect(list.tabIndex).toBe(0)
})
