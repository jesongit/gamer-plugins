import { beforeEach, expect, it, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
const mocks = vi.hoisted(() => ({ call: vi.fn() }))
vi.mock('../../../web/src/api', () => ({ api: { callExtension: mocks.call } }))
import NotifyWorkspace from './NotifyWorkspace.vue'
beforeEach(() => {
  mocks.call.mockReset().mockImplementation(async (_, action) => action === 'channels.read'
    ? { version: 'v1', channels: [{ id: 'wechat', name: '微信', kind: 'wecomlink', enabled: true, has_key: true }] }
    : action === 'records.read' ? { records: [] } : {})
})
it('编辑通道不回填密钥，空密钥保留原值且不提交 has_key', async () => {
  const w = mount(NotifyWorkspace); await flushPromises()
  await w.findAll('button').find(b => b.text() === '编辑').trigger('click')
  expect(w.get('input[type=password]').element.value).toBe('')
  await w.get('.channel-form').trigger('submit'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-notify', 'channels.save', {
    expected_version: 'v1', channel: { id: 'wechat', name: '微信', kind: 'wecomlink', enabled: true, key: '' },
  })
  w.unmount()
})
it('局域网 HTTP 没有 randomUUID 时仍可自动生成通道 ID', async () => {
  vi.stubGlobal('crypto', {})
  const w = mount(NotifyWorkspace); await flushPromises()
  await w.findAll('button').find(b => b.text() === '新增通道').trigger('click')
  await w.get('input[maxlength]').setValue('新微信')
  await w.get('input[type=password]').setValue('fixture-key')
  await w.get('.channel-form').trigger('submit'); await flushPromises()
  const saved = mocks.call.mock.calls.find(c => c[1] === 'channels.save')[2]
  expect(saved.channel.id).toMatch(/^channel-[a-z0-9-]+$/)
  expect(saved.channel.key).toBe('fixture-key')
  w.unmount(); vi.unstubAllGlobals()
})
it('测试是显式发送且只报告提交结果，离开面板不停用插件', async () => {
  const w = mount(NotifyWorkspace); await flushPromises()
  expect(mocks.call.mock.calls.every(c => c[1] !== 'notification.send')).toBe(true)
  mocks.call.mockImplementation(async (_, action) => action === 'notification.send'
    ? { accepted: true, record: { status: 'queued', message: '已加入发送队列' } }
    : action === 'channels.read' ? { version: 'v1', channels: [] } : { records: [] })
  await w.findAll('button').find(b => b.text() === '测试').trigger('click'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-notify', 'notification.send', {
    channel: 'wechat', title: 'Gamer 测试通知', content: '通知通道测试。', source: 'test',
  })
  expect(w.get('[role=status]').text()).toBe('已加入发送队列')
  w.unmount()
  expect(mocks.call.mock.calls.every(c => !['stop', 'disable'].includes(c[1]))).toBe(true)
})
