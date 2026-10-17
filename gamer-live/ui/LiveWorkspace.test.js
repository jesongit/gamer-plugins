import { beforeEach, expect, it, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
const mocks = vi.hoisted(() => ({ call: vi.fn() }))
vi.mock('../../../web/src/api', () => ({ api: { listDevices: async () => [{ id: 'phone', name: '测试手机' }], callExtension: mocks.call } }))
import LiveWorkspace from './LiveWorkspace.vue'
const mountWorkspace = () => mount(LiveWorkspace, { global: { stubs: { InteractionPanel: true, SenderSettings: true, UsersPanel: true } } })
beforeEach(() => {
  mocks.call.mockReset().mockImplementation(async (_, action) => action === 'audience.read' ? {enabled:false,url:null,text:'',version:null} : action === 'connection.settings.read' ? {version:null,mode:'',profiles:{}} : action === 'events.read' ? { events: [], next_seq: 0 } : { stream: null, connection: { state: 'disconnected' } })
})
it('打开面板只读取状态，离开不停止服务端输出', async () => {
  const w = mountWorkspace(); await flushPromises()
  expect(mocks.call.mock.calls.map(c => c[1])).toEqual(['audience.read', 'connection.settings.read', 'live.status'])
  w.unmount()
  expect(mocks.call.mock.calls.every(c => !c[1].endsWith('stop') && !c[1].endsWith('disconnect'))).toBe(true)
})
it('本机输出由显式提交启动，带设备与音频设置', async () => {
  const w = mountWorkspace(); await flushPromises()
  await w.findAll('form')[0].trigger('submit'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-live', 'stream.start', { device_id: 'phone', mode: 'local', push_url: '', audio: true, fps: 30, bitrate_kbps: 4000 })
  w.unmount()
})
it('未启动输出也能复制固定素材地址，挂载和复制不自动启动输出', async () => {
  const fixed = 'http://127.0.0.1:8443/local/extensions/gamer-live/read-key/stream/index.m3u8'
  mocks.call.mockImplementation(async (_, action) => {
    if (action === 'audience.read') return { enabled: false, url: null, text: '', version: null }
    if (action === 'connection.settings.read') return { version: null, profiles: {} }
    return { stream: null, connection: { state: 'disconnected' }, local_sources: { stream_url: fixed } }
  })
  const clipboard = vi.spyOn(navigator.clipboard, 'writeText').mockResolvedValue()
  const w = mountWorkspace()
  try {
    await flushPromises()
    expect(w.get('[aria-label="本机素材地址"]').element.value).toBe(fixed)
    await w.findAll('button').find(button => button.text() === '复制地址').trigger('click')
    await flushPromises()
    expect(clipboard).toHaveBeenCalledWith(fixed)
    expect(mocks.call.mock.calls.some(call => ['stream.start', 'automatic.start'].includes(call[1]))).toBe(false)
    expect(w.text()).toContain('地址固定')
    expect(w.text()).not.toContain('重启输出后需重新粘贴')
  } finally { w.unmount(); clipboard.mockRestore() }
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

it('四个页签分开设置、规则、用户和日志，设置同时包含输出和互动连接', async () => {
  const w = mountWorkspace(); await flushPromises()
  expect(w.findAll('.tab-btn').map(b => b.text())).toEqual(['直播设置', '互动规则', '用户管理', '触发日志'])
  expect(w.get('[aria-label="音视频输出"]').isVisible()).toBe(true)
  expect(w.get('[aria-label="互动连接"]').isVisible()).toBe(true)
  await w.get('[aria-controls="live-rules"]').trigger('click')
  expect(w.get('#live-settings').element.style.display).toBe('none')
  expect(w.findComponent({ name: 'InteractionPanel' }).props('view')).toBe('rules')
  await w.get('[aria-controls="live-users"]').trigger('click')
  expect(w.findComponent({ name: 'UsersPanel' }).exists()).toBe(true)
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

it('自动直播从服务端恢复模式，隐藏手动画面操作并保持地址可复制', async () => {
  mocks.call.mockImplementation(async (_, action) => action === 'connection.settings.read' ? { version: null, profiles: {} } : {
    stream: { state: 'streaming', source_kind: 'video_folder', source_url: 'http://127.0.0.1/live/test/index.m3u8' },
    connection: { state: 'connected' },
    automatic: { state: 'video', video_folder: 'D:\\clips', stream: { source_kind: 'video_folder', video_file: '01.mp4', video_position_secs: 12 }, queue: { target: { device_id: 'phone', package_id: 'daily' }, waiting: [] } },
  })
  const w = mountWorkspace(); await flushPromises()
  expect(w.get('[aria-label="直播模式"]').element.value).toBe('automatic')
  expect(w.get('[aria-label="直播模式"]').element.disabled).toBe(true)
  expect(w.text()).toContain('视频轮播中')
  expect(w.text()).toContain('01.mp4')
  expect(w.findAll('button').some(button => button.text() === '停止输出')).toBe(false)
  expect(w.findAll('button').some(button => button.text() === '开始输出')).toBe(false)
  expect(w.get('[aria-label="本机素材地址"]').element.value).toContain('index.m3u8')
  expect(w.findComponent({ name: 'InteractionPanel' }).props('automaticActive')).toBe(true)
  w.unmount()
})

it('已停止的自动直播恢复目录，目录请求失败保留路径并可重试', async () => {
  mocks.call.mockImplementation(async (_, action) => action === 'connection.settings.read' ? { version: null, profiles: {} } : {
    stream: null, connection: { state: 'disconnected' }, automatic: { state: 'stopped', video_folder: 'D:\\saved' },
  })
  const w = mountWorkspace(); await flushPromises()
  expect(w.get('[aria-label="直播模式"]').element.value).toBe('automatic')
  await w.get('[aria-label="视频文件夹"]').setValue('D:\\missing')
  mocks.call.mockRejectedValueOnce(new Error('视频文件夹不存在或无法读取'))
  await w.findAll('button').find(button => button.text() === '查看视频列表').trigger('click'); await flushPromises()
  expect(w.get('[role="alert"]').text()).toContain('视频文件夹不存在或无法读取')
  expect(w.get('[aria-label="视频文件夹"]').element.value).toBe('D:\\missing')
  expect(w.get('[aria-label="视频文件夹"]').element.disabled).toBe(false)
  w.unmount()
})

it('自动直播启动失败保留目录和秘密，使用互动执行目标而非手动输出设备', async () => {
  const w = mountWorkspace(); await flushPromises()
  await w.get('[aria-label="直播模式"]').setValue('automatic')
  w.findComponent({ name: 'InteractionPanel' }).vm.$emit('queue-change', { target: { device_id: 'phone', package_id: 'daily' }, waiting: [] })
  await flushPromises()
  await w.get('[aria-label="视频文件夹"]').setValue('D:\\clips')
  await w.get('[aria-label="自动直播输出方式"]').setValue('rtmp')
  await w.get('[aria-label="自动直播推流地址"]').setValue('rtmp://stream/live/secret')
  mocks.call.mockRejectedValueOnce(new Error('无法准备设备画面'))
  await w.findComponent({ name: 'AutomaticLivePanel' }).get('form').trigger('submit'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-live', 'automatic.start', expect.objectContaining({ video_folder: 'D:\\clips', device_id: 'phone', package_id: 'daily', push_url: 'rtmp://stream/live/secret' }))
  expect(w.get('[role="alert"]').text()).toContain('无法准备设备画面')
  expect(w.get('[aria-label="自动直播推流地址"]').element.value).toBe('rtmp://stream/live/secret')
  expect(w.get('[aria-label="视频文件夹"]').element.value).toBe('D:\\clips')
  w.unmount()
})

it('准备期间持续读取状态并允许停止，后返回的启动取消不覆盖停止结果', async () => {
  vi.useFakeTimers()
  let rejectStart, snapshot = {
    stream: null, connection: { state: 'disconnected' },
    automatic: { state: 'stopped', video_folder: 'D:\\clips', queue: { target: { device_id: 'phone', package_id: 'daily' }, waiting: [] } },
  }
  mocks.call.mockImplementation(async (_, action) => {
    if (action === 'connection.settings.read') return { version: null, profiles: {} }
    if (action === 'automatic.start') {
      snapshot.automatic.state = 'preparing'
      return new Promise((_, reject) => { rejectStart = reject })
    }
    if (action === 'automatic.stop') { snapshot.automatic.state = 'stopped'; return {} }
    return structuredClone(snapshot)
  })
  const w = mountWorkspace()
  try {
    await flushPromises()
    await w.findComponent({ name: 'AutomaticLivePanel' }).get('form').trigger('submit'); await flushPromises()
    await vi.advanceTimersByTimeAsync(1500); await flushPromises()
    expect(w.text()).toContain('准备直播中')
    const stop = w.findAll('button').find(button => button.text() === '停止自动直播')
    expect(stop.element.disabled).toBe(false)
    await stop.trigger('click'); await flushPromises()
    expect(mocks.call).toHaveBeenCalledWith('gamer-live', 'automatic.stop', {})
    rejectStart(new Error('自动直播启动已停止')); await flushPromises()
    expect(w.text()).not.toContain('自动直播启动已停止')
    expect(w.findComponent({ name: 'AutomaticLivePanel' }).text()).toContain('未启动')
  } finally { w.unmount(); vi.useRealTimers() }
})
