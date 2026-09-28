import { beforeEach, expect, it, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
const mocks = vi.hoisted(() => ({ call: vi.fn() }))
vi.mock('../../../web/src/api', () => ({ api: { listDevices: async () => [{ id: 'phone', name: '测试手机' }], callExtension: mocks.call } }))
import LiveWorkspace from './LiveWorkspace.vue'
const mountWorkspace = () => mount(LiveWorkspace, { global: { stubs: { InteractionPanel: true } } })
beforeEach(() => {
  mocks.call.mockReset().mockImplementation(async (_, action) => action === 'connection.settings.read' ? {version:null,mode:'',profiles:{}} : action === 'events.read' ? { events: [], next_seq: 0 } : { stream: null, connection: { state: 'disconnected' } })
})
it('打开面板只读取状态，离开不停止服务端输出', async () => {
  const w = mountWorkspace(); await flushPromises()
  expect(mocks.call.mock.calls.map(c => c[1])).toEqual(['connection.settings.read', 'live.status'])
  w.unmount()
  expect(mocks.call.mock.calls.every(c => !c[1].endsWith('stop') && !c[1].endsWith('disconnect'))).toBe(true)
})
it('本机输出由显式提交启动，带设备与音频设置', async () => {
  const w = mountWorkspace(); await flushPromises()
  await w.findAll('form')[0].trigger('submit'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-live', 'stream.start', { device_id: 'phone', mode: 'local', push_url: '', audio: true, fps: 30, bitrate_kbps: 4000 })
  w.unmount()
})
it('切换 OAuth 后不发送残留身份码，成功提交后清除秘密字段', async () => {
  const w = mountWorkspace(); await flushPromises()
  await w.get('[aria-controls="live-settings"]').trigger('click')
  await w.get('[aria-label="主播身份码"]').setValue('old-code')
  await w.get('[aria-label="应用 ID"]').setValue('123')
  await w.get('[aria-label="接入方式"]').setValue('oauth')
  await w.get('[aria-label="Access Key ID"]').setValue('key')
  await w.get('[aria-label="Access Key Secret"]').setValue('secret')
  await w.get('[aria-label="Access Token"]').setValue('token')
  await w.findAll('form')[1].trigger('submit'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-live', 'connection.connect', { platform_id: 'bilibili', expected_version: null, credentials: { mode: 'oauth', access_key: 'key', access_secret: 'secret', app_id: '', identity_code: '', access_token: 'token' } })
  expect(w.get('[aria-label="Access Key Secret"]').element.value).toBe('')
  expect(w.get('[aria-label="Access Token"]').element.value).toBe('')
  w.unmount()
})
it('错误不显示成已连接，秘密可在修正后重试', async () => {
  const w = mountWorkspace(); await flushPromises()
  await w.get('[aria-controls="live-settings"]').trigger('click')
  mocks.call.mockRejectedValueOnce(new Error('权限不足'))
  await w.findAll('form')[1].trigger('submit'); await flushPromises()
  expect(w.get('[role="alert"]').text()).toContain('权限不足')
  expect(w.text()).not.toContain('已连接')
  w.unmount()
})

it('三个页签分开设置、规则和日志，设置同时包含输出和互动连接', async () => {
  const w = mountWorkspace(); await flushPromises()
  expect(w.findAll('.tab-btn').map(b => b.text())).toEqual(['直播设置', '互动规则', '触发日志'])
  expect(w.get('[aria-label="音视频输出"]').isVisible()).toBe(true)
  expect(w.get('[aria-label="互动连接"]').isVisible()).toBe(true)
  await w.get('[aria-controls="live-rules"]').trigger('click')
  expect(w.get('#live-settings').element.style.display).toBe('none')
  expect(w.findComponent({ name: 'InteractionPanel' }).props('view')).toBe('rules')
  w.unmount()
})

it('已保存的配置回填，秘密字段留空也可重连，切换账号不沿用秘密', async () => {
  mocks.call.mockImplementation(async (_, action) => {
    if (action === 'connection.settings.read') return {version:'v1',mode:'open_live',profiles:{open_live:{access_key:'saved-key',app_id:'123',has_access_secret:true,has_identity_code:true}}}
    if (action === 'events.read') return {events:[],next_seq:0}
    return {stream:null,connection:{state:'disconnected'}}
  })
  const w=mountWorkspace(); await flushPromises()
  expect(w.get('[aria-label="Access Key ID"]').element.value).toBe('saved-key')
  expect(w.get('[aria-label="Access Key Secret"]').element.value).toBe('')
  expect(w.get('[aria-label="Access Key Secret"]').element.required).toBe(false)
  expect(w.get('[aria-label="主播身份码"]').attributes('placeholder')).toContain('已保存')
  await w.findAll('form')[1].trigger('submit'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-live','connection.connect',expect.objectContaining({expected_version:'v1',credentials:expect.objectContaining({access_secret:'',identity_code:''})}))
  await w.get('[aria-label="Access Key ID"]').setValue('other-key')
  expect(w.get('[aria-label="Access Key Secret"]').element.required).toBe(true)
  await w.get('[aria-label="接入方式"]').setValue('oauth')
  expect(w.get('[aria-label="Access Key ID"]').element.value).toBe('')
  expect(w.get('[aria-label="Access Token"]').element.required).toBe(true)
  w.unmount()
})
it('保存失败保留输入且不声称已保存', async () => {
  const w=mountWorkspace(); await flushPromises()
  await w.get('[aria-label="Access Key Secret"]').setValue('draft-secret')
  mocks.call.mockRejectedValueOnce(new Error('配置保存失败'))
  await w.findAll('button').find(b=>b.text()==='保存接入配置').trigger('click'); await flushPromises()
  expect(w.get('[role="alert"]').text()).toContain('配置保存失败')
  expect(w.get('[aria-label="Access Key Secret"]').element.value).toBe('draft-secret')
  expect(w.text()).not.toContain('接入配置已保存，下次连接无需重复输入')
  w.unmount()
})
