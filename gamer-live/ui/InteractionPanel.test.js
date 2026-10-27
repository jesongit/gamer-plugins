import { beforeEach, afterEach, expect, it, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import { newRule } from './interaction-examples'
const mock = vi.hoisted(() => ({ call: vi.fn(), events: vi.fn(), rules: [], status: {} }))
vi.mock('../../../web/src/api', () => ({ api: {
  callExtension: mock.call, listPackages: async () => [{ id: 'default', name: '默认' }],
  listScripts: async () => [{ id: 'default/test.yaml', name: 'test.yaml' }],
  listFunctions: async () => [{ functions: ['jump'] }],
  getEntrypointParams: async () => ({ schema: [{ name: 'count', type: 'integer', default: 1 }] }),
  getRunEvents: mock.events,
} }))
import InteractionPanel from './InteractionPanel.vue'
let wrapper
beforeEach(() => {
  mock.rules = []; mock.status = { revision: 1, waiting: [], history: [], receipts: [], paused: false, target: { device_id: 'phone', package_id: 'default', android_package: 'game' } }
  mock.call.mockReset().mockImplementation(async (_, action) => {
    if (action === 'logs.read') return { rows: mock.status.history.map((item, i) => ({ seq: i + 1, at: Date.now(), event: { kind: 'message', payload: { text: '跳' } }, item })), updates: [], has_more: false }
    if (action === 'queue.status') return structuredClone(mock.status)
    if (action === 'rules.read') return { rules: structuredClone(mock.rules), version: 'v1' }
    if (action === 'rules.save' || action === 'rules.toggle') return { version: 'v2' }
    if (action === 'rules.preview') return { result: '匹配成功（预览，未入队）', resolved: { args: { count: 1 } } }
    return { results: [] }
  })
  mock.events.mockReset()
})
afterEach(() => wrapper?.unmount())
async function open() { wrapper = mount(InteractionPanel, { props: { devices: [{ id: 'phone' }] } }); await flushPromises(); return wrapper }
function button(text) { return wrapper.findAll('button').find(b => b.text() === text) }
it('示例默认停用、无真实入口，保存携带资源版本', async () => {
  await open(); await button('添加示例').trigger('click'); await button('保存规则').trigger('click'); await flushPromises()
  const saved = mock.call.mock.calls.find(c => c[1] === 'rules.save')[2]
  expect(saved.expected_version).toBe('v1'); expect(saved.ruleset.rules[0]).toMatchObject({ enabled: false, entrypoint: '', value: '跳', cost: 1 })
  expect(mock.call.mock.calls.some(c => c[1] === 'queue.test')).toBe(false)
})
it('规则消耗拒绝负数、小数和超出上限，编辑后保留隐藏完成条件和确认配置', async () => {
  mock.rules = [{ ...newRule(), entrypoint: 'default/test.yaml', completion: { checkpoint_id: 'gold' }, confirmation: { timeout_secs: 300, accept: '继续' } }]
  await open(); await button('编辑').trigger('click'); await flushPromises()
  const input = wrapper.get('[aria-label="每次消耗额度"]')
  for (const value of ['-1', '1.5', '1000001', '']) {
    await input.setValue(value); await button('保存规则').trigger('click'); await flushPromises()
    expect(wrapper.get('[role="alert"]').text()).toContain('0～1000000 的整数')
  }
  expect(mock.call.mock.calls.some(c => c[1] === 'rules.save')).toBe(false)
  await input.setValue('2'); await button('保存规则').trigger('click'); await flushPromises()
  expect(mock.call.mock.calls.find(c => c[1] === 'rules.save')[2].ruleset.rules[0]).toMatchObject({ cost: 2, completion: { checkpoint_id: 'gold' }, confirmation: { timeout_secs: 300, accept: '继续' } })
})
it('免费规则读取和保存均保留 0 消耗', async () => {
  mock.rules = [{ ...newRule(), cost: 0, entrypoint: 'default/test.yaml' }]
  await open(); await button('编辑').trigger('click'); await flushPromises()
  const input = wrapper.get('[aria-label="每次消耗额度"]')
  expect(input.element.value).toBe('0'); expect(input.attributes('min')).toBe('0')
  expect(wrapper.text()).toContain('0 表示免费')
  await input.setValue('1'); await input.setValue('0')
  await button('保存规则').trigger('click'); await flushPromises()
  expect(mock.call.mock.calls.find(c => c[1] === 'rules.save')[2].ruleset.rules[0].cost).toBe(0)
})
it('模拟预览不入队，实际测试由独立操作提交', async () => {
  await open(); await button('仅预览匹配').trigger('click'); await flushPromises()
  expect(mock.call.mock.calls.some(c => c[1] === 'queue.test')).toBe(false)
  await button('加入队列测试（实际执行）').trigger('click'); await flushPromises()
  expect(mock.call).toHaveBeenCalledWith('gamer-live', 'queue.test', expect.objectContaining({ request_id: expect.any(String), payload: expect.objectContaining({ text: '跳' }) }))
})
it('规则开关立即保存，无触发总开关或启动队列按钮', async () => {
  mock.rules = [{ ...newRule({ name: '账号日常' }), entrypoint: 'default/test.yaml' }]
  await open()
  const toggle = wrapper.get('[role="switch"]')
  await toggle.setValue(true); await flushPromises()
  expect(mock.call).toHaveBeenCalledWith('gamer-live', 'rules.toggle', expect.objectContaining({ id: mock.rules[0].id, enabled: true, expected_version: 'v1' }))
  expect(toggle.element.checked).toBe(true)
  expect(wrapper.text()).not.toContain('有未保存修改')
  for (const name of ['启用新触发', '继续队列', '暂停队列', '停止互动执行']) expect(button(name)).toBeUndefined()
  mock.call.mockRejectedValueOnce(new Error('规则已被其他页面修改'))
  await toggle.setValue(false); await flushPromises()
  expect(toggle.element.checked).toBe(true)
  expect(wrapper.get('[role="alert"]').text()).toContain('其他页面修改')
})
it('按页签显示设置、规则和日志，切换保留规则草稿', async () => {
  await open()
  expect(wrapper.get('#live-rules').isVisible()).toBe(true)
  expect(wrapper.get('#live-logs').element.style.display).toBe('none')
  await button('新增规则').trigger('click')
  await wrapper.setProps({ view: 'logs' })
  expect(wrapper.get('#live-rules').element.style.display).toBe('none')
  expect(wrapper.get('#live-logs').element.style.display).not.toBe('none')
  await wrapper.setProps({ view: 'rules' })
  expect(wrapper.text()).toContain('有未保存修改')
})
it('保存冲突保留修改，日志按服务端 next 游标取到末页', async () => {
  mock.rules = [newRule({ name: '跳跃' })]
  mock.status.history = [{ id: 'q1', name: '跳跃', run_id: 'r1', state: 'failed', finished_at: Date.now() }]
  await open(); await button('新增规则').trigger('click')
  mock.call.mockRejectedValueOnce(new Error('version_conflict'))
  await button('保存规则').trigger('click'); await flushPromises()
  expect(wrapper.text()).toContain('有未保存修改'); expect(wrapper.get('[role="alert"]').text()).toContain('version_conflict')
  mock.events.mockResolvedValueOnce({ events: [{ id: 2, message: '第一页' }], next: 2, has_more: true }).mockResolvedValueOnce({ events: [{ id: 3, message: '第二页' }], next: 3, has_more: false })
  await wrapper.setProps({ view: 'logs' }); await flushPromises()
  await wrapper.get('.record').trigger('click')
  await button('查看运行日志').trigger('click'); await flushPromises()
  expect(mock.events.mock.calls).toEqual([['r1', 0], ['r1', 2]])
  expect(wrapper.text()).toContain('第二页')
})

it('自动直播锁定执行目标，队列状态发给输出面板，规则和日志仍可使用', async () => {
  mock.rules = [{ ...newRule({ name: '跳跃' }), entrypoint: 'default/test.yaml' }]
  await open()
  expect(wrapper.emitted('queue-change')[0][0].target).toEqual(mock.status.target)
  await wrapper.setProps({ view: 'settings', automaticActive: true })
  expect(wrapper.get('#live-execution-target').text()).toContain('停止自动直播后可更换')
  expect(wrapper.get('#live-execution-target select').element.disabled).toBe(true)
  expect(button('保存执行目标').element.disabled).toBe(true)
  expect(button('清除执行目标').element.disabled).toBe(true)
  await wrapper.setProps({ view: 'rules' })
  expect(wrapper.get('[role="switch"]').element.disabled).toBe(false)
  await wrapper.get('[role="switch"]').setValue(true); await flushPromises()
  expect(mock.call).toHaveBeenCalledWith('gamer-live', 'rules.toggle', expect.objectContaining({ enabled: true }))
})

it('编辑规则保留可选收尾配置和静态参数', async () => {
  const cleanup = { entrypoint: 'default/cleanup.yaml', args: { notify_failure: false }, timeout_secs: 180 }
  mock.rules = [{ ...newRule({ name: '账号日常' }), entrypoint: 'default/test.yaml', cleanup }]
  await open(); await button('编辑').trigger('click'); await flushPromises()
  await wrapper.findAll('input').find(input => input.element.value === '账号日常').setValue('修改名称')
  await button('保存规则').trigger('click'); await flushPromises()
  expect(mock.call.mock.calls.find(call => call[1] === 'rules.save')[2].ruleset.rules[0].cleanup).toEqual(cleanup)
})

it('队列收尾中保留主结果，收尾日志使用独立run id读取', async () => {
  mock.status.current = { id: 'q1', name: '日常', state: 'running', phase: 'cleanup', run_id: 'main-run', started_at: Date.now(), main_result: { state: 'success' }, cleanup: { state: 'running', run_id: 'cleanup-run' } }
  mock.events.mockResolvedValue({ events: [{ message: '重置输入' }], has_more: false })
  await open(); await wrapper.setProps({ view: 'logs' }); await flushPromises()
  expect(wrapper.text()).toContain('主任务成功 · 收尾中')
  await button('收尾日志').trigger('click'); await flushPromises()
  expect(mock.events).toHaveBeenCalledWith('cleanup-run', 0)
  expect(wrapper.text()).toContain('重置输入')
})
