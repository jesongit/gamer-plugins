import { beforeEach, expect, it, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
const mocks = vi.hoisted(() => ({ call: vi.fn() }))
vi.mock('../../../web/src/api', () => ({ api: { listDevices: async () => [{ id: 'phone', name: '测试手机' }], callExtension: mocks.call } }))
import LiveWorkspace from './LiveWorkspace.vue'
beforeEach(() => {
  mocks.call.mockReset().mockImplementation(async (_, action) => action === 'events.read' ? { events: [], next_seq: 0 } : { stream: null, connection: { state: 'disconnected' } })
})
it('打开面板只读取状态，离开不停止服务端输出', async () => {
  const w = mount(LiveWorkspace); await flushPromises()
  expect(mocks.call.mock.calls.map(c => c[1])).toEqual(['live.status', 'events.read'])
  w.unmount()
  expect(mocks.call.mock.calls.every(c => !c[1].endsWith('stop') && !c[1].endsWith('disconnect'))).toBe(true)
})
it('本机输出由显式提交启动，带设备与音频设置', async () => {
  const w = mount(LiveWorkspace); await flushPromises()
  await w.findAll('form')[0].trigger('submit'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-live', 'stream.start', { device_id: 'phone', mode: 'local', push_url: '', audio: true, fps: 30, bitrate_kbps: 4000 })
  w.unmount()
})
it('切换 OAuth 后不发送残留身份码，成功提交后清除秘密字段', async () => {
  const w = mount(LiveWorkspace); await flushPromises()
  await w.get('[aria-controls="live-interaction"]').trigger('click')
  await w.get('[aria-label="主播身份码"]').setValue('old-code')
  await w.get('[aria-label="应用 ID"]').setValue('123')
  await w.get('[aria-label="接入方式"]').setValue('oauth')
  await w.get('[aria-label="Access Key ID"]').setValue('key')
  await w.get('[aria-label="Access Key Secret"]').setValue('secret')
  await w.get('[aria-label="Access Token"]').setValue('token')
  await w.findAll('form')[1].trigger('submit'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-live', 'connection.connect', { platform_id: 'bilibili', credentials: { mode: 'oauth', access_key: 'key', access_secret: 'secret', app_id: '', identity_code: '', access_token: 'token' } })
  expect(w.get('[aria-label="Access Key Secret"]').element.value).toBe('')
  expect(w.get('[aria-label="Access Token"]').element.value).toBe('')
  w.unmount()
})
it('错误不显示成已连接，秘密可在修正后重试', async () => {
  const w = mount(LiveWorkspace); await flushPromises()
  await w.get('[aria-controls="live-interaction"]').trigger('click')
  mocks.call.mockRejectedValueOnce(new Error('权限不足'))
  await w.findAll('form')[1].trigger('submit'); await flushPromises()
  expect(w.get('[role="alert"]').text()).toContain('权限不足')
  expect(w.text()).not.toContain('已连接')
  w.unmount()
})
