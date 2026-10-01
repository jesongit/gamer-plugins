import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import { reactive } from 'vue'
import { WORKSPACE_CONTEXT_KEY } from '../../../web/src/workspace/context'
const mocks = vi.hoisted(() => ({ call: vi.fn() }))
vi.mock('../../../web/src/api', () => ({ api: { callExtension: mocks.call } }))
import AiWorkspace from './AiWorkspace.vue'

let saved, sessions, tokens, context, wrappers
const button = (wrapper, label) => wrapper.findAll('button').find(item => item.text() === label)
function makeSession(state = 'running', mode = 'api') {
  return { session_id: 's1', run_id: 'r1', device_id: 'phone', content_package: 'default', goal: '完成教程', mode,
    state, generation: 1, limits: { max_turns: 100, max_actions: 200, max_seconds: 600, max_tokens: 100000, max_failures: 3 },
    usage: { turns: 2, actions: 3, active_seconds: 4, total_tokens: null, consecutive_failures: 0 }, events: [] }
}
async function mountWorkspace() {
  const wrapper = mount(AiWorkspace, { global: { provide: { [WORKSPACE_CONTEXT_KEY]: { getSnapshot: () => context } } } })
  wrappers.push(wrapper); await flushPromises(); return wrapper
}
beforeEach(() => {
  wrappers = []; context = reactive({ deviceId: 'phone', currentPackageId: 'default', androidPackageName: 'com.demo', device: { name: '测试设备' } })
  saved = { version: 'v1', base_url: 'https://open.bigmodel.cn/api/v1', model: 'glm-5.3-flash', protocol: 'responses', request_timeout_secs: 60, has_key: true }
  sessions = []; tokens = []
  mocks.call.mockReset().mockImplementation(async (_, action, values) => {
    if (action === 'settings.get') return { ...saved }
    if (action === 'session.get') return { sessions: structuredClone(sessions) }
    if (action === 'mcp.tokens.list') return { tokens: structuredClone(tokens) }
    if (action === 'settings.save') { saved = { ...values, version: 'v2', has_key: !!values.api_key || saved.has_key }; delete saved.api_key; return { ...saved } }
    if (action === 'connection.probe') return { ok: true, protocol: saved.protocol, model: saved.model, checks: [{ name: 'image_input', ok: true }] }
    if (action === 'session.start') { sessions = [{ ...makeSession('starting', values.mode), ...values }]; return { session_id: 's1', run_id: 'r1' } }
    if (action === 'session.pause') { sessions[0].state = 'pausing'; return {} }
    if (action === 'session.resume') { sessions[0].state = 'resuming'; return {} }
    if (action === 'session.stop') { sessions[0].state = 'finished'; sessions[0].reason = 'cancelled'; return {} }
    if (action === 'mcp.tokens.create') { const value = { ...values, token_id: 't1', token: 'temporary-test-token', expires_at: 1800000000 }; tokens.push({ ...value, token: undefined }); return value }
    if (action === 'mcp.tokens.revoke') { tokens = tokens.filter(token => token.token_id !== values.token_id); return {} }
    throw new Error(`Unexpected action: ${action}`)
  })
})
afterEach(() => wrappers.forEach(wrapper => wrapper.unmount()))

it('打开面板只读取配置、会话和令牌，关闭不取消服务端运行', async () => {
  const wrapper = await mountWorkspace()
  expect(mocks.call.mock.calls.map(call => call[1])).toEqual(['settings.get', 'session.get', 'mcp.tokens.list'])
  wrapper.unmount(); wrappers = []
  expect(mocks.call.mock.calls.every(call => !call[1].includes('stop') && !call[1].includes('revoke'))).toBe(true)
})

it('开始会话使用宿主设备与配置包，不从Android包名推导；提交完整预算', async () => {
  const wrapper = await mountWorkspace()
  await wrapper.get('[aria-label="目标描述"]').setValue('  完成新手教程  ')
  await wrapper.get('#ai-play form').trigger('submit'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'session.start', {
    device_id: 'phone', content_package: 'default', goal: '完成新手教程', mode: 'api',
    limits: { max_turns: 40, max_actions: 120, max_seconds: 600, max_tokens: 100000, max_failures: 3 },
  })
  expect(wrapper.get('[data-testid="session-state"]').text()).toBe('准备中')
  context.currentPackageId = ''; await flushPromises()
  expect(wrapper.get('#ai-play form button[type="submit"]').element.disabled).toBe(true)
})

it('暂停请求完成前仍锁定人工提示，只在paused后显示继续；停止保留终态', async () => {
  sessions = [makeSession()]
  const wrapper = await mountWorkspace()
  expect(wrapper.get('[data-testid="control-hint"]').text()).toContain('人工操作已锁定')
  await button(wrapper, '暂停 AI').trigger('click'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'session.pause', { session_id: 's1' })
  expect(wrapper.get('[data-testid="session-state"]').text()).toBe('正在暂停')
  expect(wrapper.get('[data-testid="control-hint"]').text()).toContain('完成前人工操作仍被锁定')
  expect(button(wrapper, '继续 AI')).toBeUndefined()
  expect(button(wrapper, '暂停 AI')).toBeUndefined()
})

it('暂停完成允许人工提示，恢复不新建run，恢复阶段再次锁定', async () => {
  sessions = [makeSession('paused')]
  const wrapper = await mountWorkspace()
  expect(wrapper.get('[data-testid="session-state"]').text()).toContain('可人工操作')
  expect(button(wrapper, '暂停 AI')).toBeUndefined()
  await button(wrapper, '继续 AI').trigger('click'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'session.resume', { session_id: 's1' })
  expect(mocks.call.mock.calls.some(call => call[1] === 'session.start')).toBe(false)
  expect(wrapper.get('[data-testid="session-state"]').text()).toBe('正在恢复')
  expect(wrapper.get('[data-testid="control-hint"]').text()).toContain('人工操作已锁定')
  await button(wrapper, '停止会话').trigger('click'); await flushPromises()
  expect(wrapper.get('[data-testid="session-state"]').text()).toBe('已结束')
  expect(wrapper.text()).toContain('cancelled')
})

it('连接能力测试尚未返回时，仍可暂停和停止正在运行的会话', async () => {
  sessions = [makeSession()]
  const wrapper = await mountWorkspace()
  const implementation = mocks.call.getMockImplementation()
  let finishProbe
  mocks.call.mockImplementation((id, action, values) => action === 'connection.probe'
    ? new Promise(resolve => { finishProbe = resolve }) : implementation(id, action, values))
  await wrapper.get('[aria-controls="ai-settings"]').trigger('click')
  await button(wrapper, '测试已保存配置').trigger('click'); await flushPromises()
  expect(wrapper.text()).toContain('测试已保存的连接…')
  await wrapper.get('[aria-controls="ai-play"]').trigger('click')
  expect(button(wrapper, '暂停 AI').element.disabled).toBe(false)
  expect(button(wrapper, '停止会话').element.disabled).toBe(false)
  await button(wrapper, '暂停 AI').trigger('click'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'session.pause', { session_id: 's1' })
  expect(wrapper.get('[data-testid="session-state"]').text()).toBe('正在暂停')
  expect(button(wrapper, '停止会话').element.disabled).toBe(false)
  await button(wrapper, '停止会话').trigger('click'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'session.stop', { session_id: 's1' })
  expect(wrapper.get('[data-testid="session-state"]').text()).toBe('已结束')
  expect(wrapper.text()).toContain('测试已保存的连接…')
  finishProbe({ ok: true, protocol: 'responses', model: 'glm-5.3-flash', checks: [] })
  await flushPromises()
})

it('连接能力测试尚未返回时，轮询仍更新会话状态、预算与截图记录', async () => {
  vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
  try {
    sessions = [makeSession()]
    const wrapper = await mountWorkspace()
    const implementation = mocks.call.getMockImplementation()
    let finishProbe
    mocks.call.mockImplementation((id, action, values) => action === 'connection.probe'
      ? new Promise(resolve => { finishProbe = resolve }) : implementation(id, action, values))
    await wrapper.get('[aria-controls="ai-settings"]').trigger('click')
    await button(wrapper, '测试已保存配置').trigger('click'); await flushPromises()
    sessions[0].state = 'paused'
    sessions[0].usage.turns = 7
    sessions[0].events.push({ seq: 1, kind: 'observation', message: '测试期间的新观察',
      data: { image_data_url: 'data:image/png;base64,AA==' } })
    await vi.advanceTimersByTimeAsync(1500); await flushPromises()
    expect(wrapper.get('[data-testid="session-state"]').text()).toContain('可人工操作')
    expect(wrapper.get('.usage-grid').text()).toContain('7 / 100')
    expect(wrapper.get('.events').text()).toContain('测试期间的新观察')
    expect(wrapper.get('figure img').attributes('src')).toBe('data:image/png;base64,AA==')
    expect(button(wrapper, '继续 AI').element.disabled).toBe(false)
    expect(wrapper.text()).toContain('测试已保存的连接…')
    finishProbe({ ok: true, protocol: 'responses', model: 'glm-5.3-flash', checks: [] })
    await flushPromises()
  } finally {
    wrappers.forEach(wrapper => wrapper.unmount()); wrappers = []
    vi.useRealTimers()
  }
})

it('切换工作台上下文不会改写活动会话；缺失token用量显示未知', async () => {
  sessions = [makeSession('paused')]
  const wrapper = await mountWorkspace()
  expect(wrapper.get('.usage-grid').text()).toContain('未知')
  context.deviceId = 'browser'; context.currentPackageId = 'other'; await flushPromises()
  expect(wrapper.text()).toContain('此会话绑定设备 phone / 配置包 default')
  await button(wrapper, '继续 AI').trigger('click'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'session.resume', { session_id: 's1' })
})

it('协议选择不偷偷换端点，显式填入后保存；成功清空密钥并测试保存版本', async () => {
  const wrapper = await mountWorkspace()
  await wrapper.get('[aria-controls="ai-settings"]').trigger('click')
  await wrapper.get('[aria-label="API 协议"]').setValue('chat_completions')
  expect(wrapper.get('[aria-label="API 基础地址"]').element.value).toBe('https://open.bigmodel.cn/api/v1')
  await button(wrapper, '填入此地址').trigger('click')
  await wrapper.get('[aria-label="API 密钥"]').setValue('draft-test-key')
  await button(wrapper, '保存并测试连接').trigger('click'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'settings.save', {
    expected_version: 'v1', base_url: 'https://open.bigmodel.cn/api/paas/v4', model: 'glm-5.3-flash',
    protocol: 'chat_completions', request_timeout_secs: 60, api_key: 'draft-test-key',
  })
  expect(wrapper.get('[aria-label="API 密钥"]').element.value).toBe('')
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'connection.probe', {})
  expect(wrapper.get('[aria-label="连接测试结果"]').text()).toContain('图片识别')
  expect(wrapper.text()).toContain('图片与工具闭环测试通过')
})

it('保存失败保留密钥草稿，不声称保存成功', async () => {
  const wrapper = await mountWorkspace()
  await wrapper.get('[aria-controls="ai-settings"]').trigger('click')
  await wrapper.get('[aria-label="API 密钥"]').setValue('draft-test-key')
  const implementation = mocks.call.getMockImplementation()
  mocks.call.mockImplementation((id, action, values) => action === 'settings.save' ? Promise.reject(new Error('版本冲突')) : implementation(id, action, values))
  await wrapper.get('#ai-settings form').trigger('submit'); await flushPromises()
  expect(wrapper.get('[role="alert"]').text()).toContain('版本冲突')
  expect(wrapper.get('[aria-label="API 密钥"]').element.value).toBe('draft-test-key')
  expect(wrapper.text()).not.toContain('连接设置已保存。')
})

it('外部MCP不要求API密钥，控制令牌需已建立同目标的外部会话', async () => {
  saved.has_key = false
  const wrapper = await mountWorkspace()
  await wrapper.get('[aria-controls="ai-mcp"]').trigger('click')
  await wrapper.get('[aria-label="授权范围"]').setValue('true')
  expect(button(wrapper, '创建连接令牌').element.disabled).toBe(true)
  await wrapper.get('[aria-controls="ai-play"]').trigger('click')
  await wrapper.get('[aria-label="控制方式"]').setValue('mcp')
  await wrapper.get('[aria-label="目标描述"]').setValue('通过外部客户端控制')
  expect(button(wrapper, '建立外部控制会话').element.disabled).toBe(false)
  await wrapper.get('#ai-play form').trigger('submit'); await flushPromises()
  await wrapper.get('[aria-controls="ai-mcp"]').trigger('click')
  expect(button(wrapper, '创建连接令牌').element.disabled).toBe(false)
  await wrapper.get('#ai-mcp form').trigger('submit'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'mcp.tokens.create', {
    label: '', device_id: 'phone', content_package: 'default', control: true, ttl_seconds: 120,
  })
  expect(wrapper.get('[aria-label="新连接令牌"]').element.value).toBe('temporary-test-token')
  expect(wrapper.get('#ai-mcp pre').text()).toContain('Authorization')
  await button(wrapper, '撤销').trigger('click'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'mcp.tokens.revoke', { token_id: 't1' })
  expect(wrapper.find('[aria-label="新连接令牌"]').exists()).toBe(false)
})

it('观察记录只展示真实data图片，事件详情去掉秘密及大块图片数据', async () => {
  sessions = [{ ...makeSession(), events: [
    { seq: 1, kind: 'capture', message: '已截图', data: { image_data_url: 'data:image/png;base64,AA==', width: 128, api_key: 'never-display', nested: { authorization: 'secret', x: 4 } } },
    { seq: 2, kind: 'capture', message: '非法图片', data: { image_data_url: 'javascript:alert(1)' } },
  ] }]
  const wrapper = await mountWorkspace()
  expect(wrapper.get('figure img').attributes('src')).toBe('data:image/png;base64,AA==')
  expect(wrapper.get('.events pre').text()).toContain('128')
  expect(wrapper.get('.events pre').text()).not.toContain('never-display')
  expect(wrapper.get('.events pre').text()).not.toContain('AA==')
  expect(wrapper.get('.events pre').text()).not.toContain('secret')
})
