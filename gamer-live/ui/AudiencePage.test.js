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
  expect(document.querySelector('#featured').textContent).toContain('观众88')
  expect(document.querySelector('#featured').textContent).not.toContain('观众87')
  expect(document.querySelector('#featured').textContent).not.toContain('已进行')
  expect(document.querySelector('#featured').children).toHaveLength(2)
  expect(document.querySelectorAll('#waiting li')).toHaveLength(12)
  expect([...document.querySelectorAll('#waiting .position')].map(node => node.textContent)).toEqual(Array.from({ length: 12 }, (_, index) => String(index + 1).padStart(2, '0')))
  expect(document.querySelector('#queue-total').textContent).toBe('12 项等待')
  expect(document.querySelector('#toast')).toBeNull()
})
it('当前任务两行显示姓名和额度，隐藏身份明细并将确认倒计时放进状态行', async () => {
  const openId = '完整标识ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789'
  snapshot.current = item(88, 'running', { viewer: '名字很长的观众', open_id: openId, medal_name: '星海', medal_level: 16, phase: 'awaiting_confirmation', confirmation: { state: 'awaiting_confirmation', deadline_at: snapshot.time + 300000 }, quota: { remaining: 4, reserved: 2, available: 2, unlimited: false } })
  const document = await open()
  expect(document.querySelector('#featured').dataset.state).toBe('awaiting_confirmation')
  expect(document.querySelector('#featured .medal')).toBeNull()
  expect(document.querySelector('#featured .task-title').textContent).toBe('名字很长的观众')
  expect(document.querySelector('#featured .open-id')).toBeNull()
  expect(document.querySelector('#featured').textContent).not.toContain(openId)
  expect(document.querySelector('#featured').children).toHaveLength(2)
  expect(document.querySelector('.task-status #confirmation-time')).not.toBeNull()
  expect(document.querySelector('#featured .quota').textContent).toBe('剩余 2 次')
  expect(document.querySelector('#confirmation-time').textContent).toBe('确认剩余 5:00')
  snapshot.time += 1000
  await vi.advanceTimersByTimeAsync(1000); await flushPromises()
  expect(document.querySelector('#confirmation-time').textContent).toBe('确认剩余 4:59')
  await update({ current: item(89, 'running', { quota: { unlimited: true, reserved: 1 } }) })
  expect(document.querySelector('#featured .quota').textContent).toBe('不限次')
  expect(document.querySelector('#confirmation-time')).toBeNull()
})
it('等待队列仅保留两行摘要，隐藏标识和粉丝牌，外部文字保持纯文本', async () => {
  const injected = '<img src=x onerror=alert(1)>'
  snapshot.waiting = [item(90, 'waiting', { viewer: injected, open_id: injected, medal_name: injected, medal_level: 8, quota: { remaining: 1, reserved: 1, available: 0 } })]
  const document = await open()
  expect(document.querySelector('#waiting .open-id')).toBeNull()
  expect(document.querySelector('#waiting .medal')).toBeNull()
  expect(document.querySelector('#waiting .waiting-title').textContent).toBe(injected)
  expect(document.querySelector('.waiting-content').children).toHaveLength(2)
  expect(document.querySelector('#waiting .quota').textContent).toBe('剩余 0 次')
  expect(document.querySelectorAll('img')).toHaveLength(0)
})
it('扫码与重新扫码显示各自阶段及剩余期限，不再伪装成执行中', async () => {
  snapshot.current = item(88, 'running', { phase: 'awaiting_scan', confirmation: { state: 'awaiting_scan', deadline_at: snapshot.time + 300000 } })
  const document = await open()
  expect(document.querySelector('.task-status').textContent).toBe('扫码剩余 5:00')
  expect(document.querySelector('#confirmation-time').textContent).toBe('扫码剩余 5:00')
  await update({ current: item(88, 'running', { phase: 'rescanning', confirmation: { state: 'rescan_requested', deadline_at: snapshot.time + 40000 } }) })
  expect(document.querySelector('.task-status').textContent).toContain('重扫剩余')
  expect(document.querySelector('#confirmation-time').textContent).toContain('重扫剩余')
  await update({ current: item(88, 'running', { phase: 'cleanup', confirmation: { state: 'timeout' } }) })
  expect(document.querySelector('.task-status').textContent).toBe('退出中')
  expect(document.querySelector('#confirmation-time')).toBeNull()
})

it('同一排队观众的剩余可用次数随赠礼入账、预占和结算更新，无限次数明确显示', async () => {
  const waiting = item(90, 'waiting', { quota: { remaining: 1, reserved: 1, available: 0, unlimited: false } })
  snapshot.waiting = [waiting]; snapshot.waiting_count = 1
  const document = await open()
  expect(document.querySelector('#waiting .quota').textContent).toBe('剩余 0 次')
  await update({ waiting: [{ ...waiting, quota: { remaining: 3, reserved: 1, available: 2, unlimited: false } }] })
  expect(document.querySelector('#waiting .quota').textContent).toBe('剩余 2 次')
  await update({ waiting: [{ ...waiting, quota: { remaining: 3, reserved: 2, available: 1, unlimited: false } }] })
  expect(document.querySelector('#waiting .quota').textContent).toBe('剩余 1 次')
  await update({ waiting: [{ ...waiting, quota: { remaining: 2, reserved: 1, available: 1, unlimited: false } }] })
  expect(document.querySelector('#waiting .quota').textContent).toBe('剩余 1 次')
  await update({ waiting: [{ ...waiting, quota: { reserved: 1, unlimited: true } }] })
  expect(document.querySelector('#waiting .quota').textContent).toBe('不限次')
})

it('失败、结束和收尾优先于残留登录状态，不继续显示扫码倒计时', async () => {
  const confirmation = { state: 'awaiting_scan', deadline_at: snapshot.time + 300000 }
  snapshot.current = item(88, 'running', { phase: 'awaiting_scan', confirmation })
  const document = await open()
  expect(document.querySelector('#confirmation-time')).not.toBeNull()
  await update({ current: item(88, 'running', { phase: 'cleanup', confirmation }) })
  expect(document.querySelector('.task-status').textContent).toBe('退出中')
  expect(document.querySelector('#confirmation-time')).toBeNull()
  for (const [state, label] of [['failed', '失败'], ['cancelled', '已取消'], ['success', '已完成']]) {
    await update({ current: null, featured: item(88, state, { phase: 'finished', confirmation }) })
    expect(document.querySelector('#activity').textContent).toBe('空闲中')
    expect(document.querySelector('.task-status').textContent).toBe(label)
    expect(document.querySelector('#confirmation-time')).toBeNull()
  }
})

it('当前和排队项展示本次消耗，0 或入队时免扣显示免费', async () => {
  snapshot.current = item(88, 'running', { cost: 2, billing_state: 'reserved', quota: { unlimited: true, reserved: 2 } })
  snapshot.waiting = [item(89, 'waiting', { cost: 0, billing_state: 'exempt' }), item(90, 'waiting', { cost: 3, billing_state: 'reserved' }), item(91, 'waiting', { cost: 5, billing_state: 'exempt' })]
  const document = await open()
  expect(document.querySelector('#featured .cost').textContent).toBe('本次消耗 2 次')
  expect([...document.querySelectorAll('#waiting .cost')].map(n => n.textContent)).toEqual(['免费', '消耗 3 次', '免费'])
  await update({ current: snapshot.waiting[0], waiting: [snapshot.waiting[1]], featured: null })
  expect(document.querySelector('#featured .cost').textContent).toBe('免费')
  expect(document.querySelector('#waiting .cost').textContent).toBe('消耗 3 次')
  await update({ current: null, featured: item(89, 'success', { cost: 0, billing_state: 'exempt' }), waiting: [] })
  expect(document.querySelector('#featured .cost').textContent).toBe('免费')
})

it('未提供有效消耗时不误报免费，也不展示任意文本', async () => {
  snapshot.waiting = [undefined, null, -1, 1.5, '0', '<img src=x onerror=alert(1)>'].map((cost, i) => item(i + 1, 'waiting', { cost }))
  const document = await open()
  expect(document.querySelectorAll('.cost')).toHaveLength(0)
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
  expect(document.querySelector('#featured').textContent).not.toContain('观众20')
  expect(document.querySelector('.task-status').textContent).toBe('失败')
  await update({ featured: item(22, 'cancelled') })
  expect(document.querySelector('.task-status').textContent).toBe('已取消')
  expect(document.querySelector('.waiting-section').hidden).toBe(true)
})

it('空队列没有假任务，公开名称和多行自定义文字按纯文本展示', async () => {
  const document = await open()
  expect(document.querySelector('#featured').textContent).toContain('等待互动')
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
  expect(document.querySelector('#featured').textContent).toContain('观众8')
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
