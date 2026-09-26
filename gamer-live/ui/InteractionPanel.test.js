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
  mock.rules = []; mock.status = { revision: 1, waiting: [], history: [], receipts: [], paused: true, enabled: false, target: { device_id: 'phone', package_id: 'default', android_package: 'game' } }
  mock.call.mockReset().mockImplementation(async (_, action) => {
    if (action === 'queue.status') return structuredClone(mock.status)
    if (action === 'rules.read') return { rules: structuredClone(mock.rules), version: 'v1' }
    if (action === 'rules.save') return { version: 'v2' }
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
  expect(saved.expected_version).toBe('v1'); expect(saved.ruleset.rules[0]).toMatchObject({ enabled: false, entrypoint: '', value: '跳' })
  expect(mock.call.mock.calls.some(c => c[1] === 'queue.test')).toBe(false)
})
it('模拟预览不入队，实际测试由独立操作提交', async () => {
  await open(); await button('仅预览匹配').trigger('click'); await flushPromises()
  expect(mock.call.mock.calls.some(c => c[1] === 'queue.test')).toBe(false)
  await button('加入队列测试（实际执行）').trigger('click'); await flushPromises()
  expect(mock.call).toHaveBeenCalledWith('gamer-live', 'queue.test', expect.objectContaining({ request_id: expect.any(String), payload: expect.objectContaining({ text: '跳' }) }))
})
it('暂停不取消当前运行，服务端错误在页面保留', async () => {
  mock.status.paused = false; await open()
  await button('暂停队列').trigger('click'); await flushPromises()
  expect(mock.call).toHaveBeenCalledWith('gamer-live', 'queue.control', expect.objectContaining({ op: 'pause' }))
  mock.call.mockRejectedValueOnce(new Error('保存失败，已暂停'))
  await button('停止互动执行').trigger('click'); await flushPromises()
  expect(wrapper.get('[role="alert"]').text()).toContain('保存失败')
})
it('保存冲突保留修改，日志按服务端 next 游标取到末页', async () => {
  mock.rules = [newRule({ name: '跳跃' })]
  mock.status.history = [{ id: 'q1', name: '跳跃', run_id: 'r1', state: 'failed', finished_at: Date.now() }]
  await open(); await button('新增规则').trigger('click')
  mock.call.mockRejectedValueOnce(new Error('version_conflict'))
  await button('保存规则').trigger('click'); await flushPromises()
  expect(wrapper.text()).toContain('有未保存修改'); expect(wrapper.get('[role="alert"]').text()).toContain('version_conflict')
  mock.events.mockResolvedValueOnce({ events: [{ id: 2, message: '第一页' }], next: 2, has_more: true }).mockResolvedValueOnce({ events: [{ id: 3, message: '第二页' }], next: 3, has_more: false })
  await button('详情与日志').trigger('click'); await flushPromises()
  expect(mock.events.mock.calls).toEqual([['r1', 0], ['r1', 2]])
  expect(wrapper.text()).toContain('第二页')
})
