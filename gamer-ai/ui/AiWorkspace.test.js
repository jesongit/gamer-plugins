import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import { reactive } from 'vue'
import { WORKSPACE_CONTEXT_KEY } from '../../../web/src/workspace/context'
const mocks = vi.hoisted(() => ({ call: vi.fn() }))
vi.mock('../../../web/src/api', () => ({ api: { callExtension: mocks.call } }))
import AiWorkspace from './GameSessionPane.vue'

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
    if (action === 'session.start') { sessions = [{ ...makeSession('starting', values.mode), ...values }]; return { session_id: 's1', run_id: 'r1', conversation_id: 's1' } }
    if (action === 'session.pause') { sessions[0].state = 'pausing'; return {} }
    if (action === 'session.resume') { sessions[0].state = 'resuming'; if (values.limits) sessions[0].limits = values.limits; sessions[0].usage.consecutive_failures = 0; return {} }
    if (action === 'session.message') {
      const message = { id: 'm2', role: 'user', text: values.message, at: '2026-10-03T12:01:00Z' }
      sessions[0].messages ||= [{ id: 'm1', role: 'user', text: sessions[0].goal, at: '2026-10-03T12:00:00Z' }]
      sessions[0].messages.push(message)
      sessions[0].events.push({ seq: sessions[0].events.length + 1, kind: 'user', message: message.text, at: message.at, data: { message_id: message.id } })
      if (values.limits) sessions[0].limits = values.limits
      sessions[0].state = values.resume ? 'running' : 'paused'
      return { session: structuredClone(sessions[0]), resumed: values.resume }
    }
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
  await wrapper.get('[aria-label="消息"]').setValue('  完成新手教程  ')
  await wrapper.get('#ai-play form').trigger('submit'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'session.start', {
    device_id: 'phone', content_package: 'default', goal: '完成新手教程', mode: 'api',
    limits: { max_turns: 40, max_actions: 120, max_seconds: 600, max_tokens: 100000, max_failures: 3 },
  })
  expect(wrapper.get('[data-testid="session-state"]').text()).toBe('准备中')
  expect(wrapper.emitted('session-start')[0][0]).toEqual({conversation_id:'s1'})
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
    protocol: 'chat_completions', request_timeout_secs: 60, max_output_tokens:16384, public_reasoning_content: true, api_key: 'draft-test-key',
  })
  expect(wrapper.get('[aria-label="API 密钥"]').element.value).toBe('')
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'connection.probe', {})
  expect(wrapper.get('[aria-label="连接测试结果"]').text()).toContain('图片识别')
  expect(wrapper.text()).toContain('图片与工具闭环测试通过')
})

it('公开思考默认展示，跨协议保留选择且单次输出0独立于累计预算', async () => {
  const wrapper = await mountWorkspace()
  await wrapper.get('[aria-controls="ai-settings"]').trigger('click')
  const toggle = wrapper.get('[aria-label="显示供应商公开思考"]')
  expect(toggle.element.checked).toBe(true)
  expect(toggle.element.disabled).toBe(false)
  await wrapper.get('[aria-label="API 协议"]').setValue('chat_completions')
  await wrapper.get('[aria-label="单次输出 Token 上限"]').setValue(0)
  await wrapper.get('#ai-settings form').trigger('submit'); await flushPromises()
  expect(mocks.call.mock.calls.find(call => call[1] === 'settings.save')[2].public_reasoning_content).toBe(true)
  expect(mocks.call.mock.calls.find(call => call[1] === 'settings.save')[2].max_output_tokens).toBe(0)
  expect(toggle.element.checked).toBe(true)
  await wrapper.get('[aria-label="API 协议"]').setValue('responses')
  expect(toggle.element.checked).toBe(true)
  expect(toggle.element.disabled).toBe(false)
  await toggle.setValue(false)
  await wrapper.get('#ai-settings form').trigger('submit'); await flushPromises()
  expect(mocks.call.mock.calls.filter(call => call[1] === 'settings.save').at(-1)[2].public_reasoning_content).toBe(false)
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
  await wrapper.get('[aria-label="控制方式"]').setValue('mcp')
  await wrapper.get('[aria-label="消息"]').setValue('通过外部客户端控制')
  expect(button(wrapper, '建立外部控制会话').element.disabled).toBe(false)
  await wrapper.get('#ai-play form').trigger('submit'); await flushPromises()
  await wrapper.get('[aria-controls="ai-mcp"]').trigger('click')
  expect(button(wrapper, '创建连接令牌').element.disabled).toBe(false)
  await wrapper.get('#ai-mcp form').trigger('submit'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'mcp.tokens.create', {
    label: '', device_id: 'phone', content_package: 'default', control: true, ttl_seconds: 120,
    memory_read:false,memory_write:false,protected_write:false,web_search:false,
  })
  expect(wrapper.get('[aria-label="新连接令牌"]').element.value).toBe('temporary-test-token')
  expect(wrapper.get('#ai-mcp pre').text()).toContain('Authorization')
  await button(wrapper, '撤销').trigger('click'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'mcp.tokens.revoke', { token_id: 't1' })
  expect(wrapper.find('[aria-label="新连接令牌"]').exists()).toBe(false)
})

it('无设备MCP可单独授权本包记忆，保护字段要求维护权限，不创建或恢复设备会话', async () => {
  context.deviceId=''
  const wrapper=await mountWorkspace()
  await wrapper.get('[aria-controls="ai-mcp"]').trigger('click')
  await wrapper.get('[aria-label="令牌设备绑定"]').setValue('none')
  expect(button(wrapper,'创建连接令牌').element.disabled).toBe(true)
  const check=label=>wrapper.findAll('label').find(item=>item.text().includes(label)).get('input[type="checkbox"]')
  expect(check('明确允许修改人工保护字段').element.disabled).toBe(true)
  await check('允许读取本配置包记忆').setValue(true)
  await wrapper.get('#ai-mcp form').trigger('submit');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','mcp.tokens.create',{label:'',device_id:'',content_package:'default',control:false,ttl_seconds:120,memory_read:true,memory_write:false,protected_write:false,web_search:false})
  await check('允许 AI 维护本配置包记忆').setValue(true)
  await check('明确允许修改人工保护字段').setValue(true)
  await check('允许 AI 维护本配置包记忆').setValue(false)
  expect(check('明确允许修改人工保护字段').element.checked).toBe(false)
  expect(mocks.call.mock.calls.some(call=>['session.start','session.resume'].includes(call[1]))).toBe(false)
})

it('记忆维护只自动补齐读取，取消读取收回维护及保护权限，保留独立联网选择', async () => {
  context.deviceId=''
  const wrapper=await mountWorkspace()
  await wrapper.get('[aria-controls="ai-mcp"]').trigger('click')
  await wrapper.get('[aria-label="令牌设备绑定"]').setValue('none')
  const check=label=>wrapper.findAll('label').find(item=>item.text().includes(label)).get('input[type="checkbox"]')
  await check('允许 AI 维护本配置包记忆').setValue(true)
  expect(check('允许读取本配置包记忆').element.checked).toBe(true)
  expect(check('明确允许修改人工保护字段').element.checked).toBe(false)
  expect(check('允许使用已配置的独立联网服务').element.checked).toBe(false)
  await check('明确允许修改人工保护字段').setValue(true)
  await check('允许使用已配置的独立联网服务').setValue(true)
  await check('允许读取本配置包记忆').setValue(false)
  expect(check('允许 AI 维护本配置包记忆').element.checked).toBe(false)
  expect(check('明确允许修改人工保护字段').element.checked).toBe(false)
  expect(check('明确允许修改人工保护字段').element.disabled).toBe(true)
  expect(check('允许使用已配置的独立联网服务').element.checked).toBe(true)
  await wrapper.get('#ai-mcp form').trigger('submit');await flushPromises()
  expect(mocks.call.mock.calls.find(call=>call[1]==='mcp.tokens.create')[2]).toMatchObject({device_id:'',control:false,memory_read:false,memory_write:false,protected_write:false,web_search:true})
  await check('允许 AI 维护本配置包记忆').setValue(true)
  await wrapper.get('#ai-mcp form').trigger('submit');await flushPromises()
  expect(mocks.call.mock.calls.filter(call=>call[1]==='mcp.tokens.create').at(-1)[2]).toMatchObject({device_id:'',control:false,memory_read:true,memory_write:true,protected_write:false,web_search:true})
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

it('运行中发送新指令明确打断后继续，使用同一会话且保留原始目标', async () => {
  sessions = [makeSession()]
  const wrapper = await mountWorkspace()
  expect(wrapper.get('#ai-play').text()).toContain('发送新指令会打断本轮')
  await wrapper.get('[aria-label="消息"]').setValue('先检查右上角的设置按钮')
  await wrapper.get('#ai-play form').trigger('submit'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'session.message', { session_id: 's1', message: '先检查右上角的设置按钮', resume: true })
  expect(mocks.call.mock.calls.some(call => call[1] === 'session.start' || call[1] === 'session.resume')).toBe(false)
  expect(sessions[0].goal).toBe('完成教程')
  expect(wrapper.findAll('.message-user').map(item => item.text().includes('先检查')).filter(Boolean)).toHaveLength(1)
  expect(wrapper.findAll('.message-user')).toHaveLength(2)
  expect(wrapper.get('[aria-label="消息"]').element.value).toBe('')
})

it('暂停时可仅发送指令，保持暂停且不静默恢复', async () => {
  sessions = [makeSession('paused')]
  const wrapper = await mountWorkspace()
  await wrapper.get('[aria-label="消息"]').setValue('我先人工调整，请等候')
  await button(wrapper, '仅发送，保持暂停').trigger('click'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'session.message', { session_id: 's1', message: '我先人工调整，请等候', resume: false })
  expect(wrapper.get('[data-testid="session-state"]').text()).toContain('已暂停')
  expect(wrapper.text()).toContain('消息已接收，会话保持暂停')
})

it('消息被接收但恢复失败时保留气泡并清草稿，说明仍暂停', async () => {
  sessions = [makeSession('paused')]
  const wrapper = await mountWorkspace()
  const implementation = mocks.call.getMockImplementation()
  mocks.call.mockImplementation(async (id, action, values) => {
    if (action !== 'session.message') return implementation(id, action, values)
    const reply = await implementation(id, action, { ...values, resume: false })
    return { ...reply, resume_error: '目标已断开' }
  })
  await wrapper.get('[aria-label="消息"]').setValue('连接恢复后再继续')
  await wrapper.get('#ai-play form').trigger('submit'); await flushPromises()
  expect(wrapper.get('[role="alert"]').text()).toContain('消息已接收，恢复未完成：目标已断开')
  expect(wrapper.get('[aria-label="消息"]').element.value).toBe('')
  expect(wrapper.findAll('.message-user').some(item => item.text().includes('连接恢复后再继续'))).toBe(true)
  expect(wrapper.get('[data-testid="session-state"]').text()).toContain('已暂停')
})

it('预算暂停显示已知token下限，提高上限时才提交完整新预算', async () => {
  sessions = [{ ...makeSession('paused'), reason: 'Token 使用达到预算', usage: { ...makeSession().usage, known_tokens: 105396, has_unknown_tokens: true } }]
  const wrapper = await mountWorkspace()
  expect(wrapper.get('.message-pause').text()).toContain('至少 105,396 / 100,000')
  expect(wrapper.get('.message-pause').text()).toContain('部分请求用量未知')
  expect(button(wrapper, '继续 AI').element.disabled).toBe(true)
  await button(wrapper, '调整运行预算').trigger('click')
  expect(wrapper.get('[aria-label="最大模型轮数（0 表示无上限）"]').element.value).toBe('100')
  await wrapper.get('[aria-label="累计 token 上限（0 表示无上限）"]').setValue(200000)
  await button(wrapper, '调整预算并继续').trigger('click'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'session.resume', { session_id: 's1', limits: { max_turns: 100, max_actions: 200, max_seconds: 600, max_tokens: 200000, max_failures: 3 } })
})

it('token上限可设0解除预算暂停，所有预算输入均允许0', async () => {
  sessions = [{ ...makeSession('paused'), reason: 'Token 使用达到预算', usage: { ...makeSession().usage, known_tokens: 105396, has_unknown_tokens: true } }]
  const wrapper = await mountWorkspace()
  await button(wrapper, '调整运行预算').trigger('click')
  const tokenBudget = wrapper.get('[aria-label="累计 token 上限（0 表示无上限）"]')
  expect(tokenBudget.attributes('min')).toBe('0')
  expect(wrapper.get('[aria-label="最大模型轮数（0 表示无上限）"]').attributes('min')).toBe('0')
  await tokenBudget.setValue(0)
  expect(button(wrapper, '继续 AI').element.disabled).toBe(false)
  await wrapper.get('[aria-label="消息"]').setValue('提高预算，继续当前任务')
  await wrapper.get('#ai-play form').trigger('submit'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'session.message', { session_id: 's1', message: '提高预算，继续当前任务', resume: true,
    limits: { max_turns: 100, max_actions: 200, max_seconds: 600, max_tokens: 0, max_failures: 3 } })
  expect(wrapper.get('.usage-grid').text()).toContain('不限')
})

it('时间线合并工具进度与结果、公开摘要，隐藏私密推理并折叠截图', async () => {
  sessions = [{ ...makeSession(), messages: [{ id: 'm1', role: 'user', text: '完成教程', at: '2026-10-03T12:00:00Z' }], events: [
    { seq: 1, kind: 'user', message: '完成教程', at: '2026-10-03T12:00:00Z', data: { message_id: 'm1' } },
    { seq: 2, kind: 'decision', message: '先观察按钮位置，再进行点击', data: { category: 'summary', generation: 1 } },
    { seq: 3, kind: 'tool_start', message: '正在点击', data: { call_id: 'c1', tool: 'input_tap', arguments: { x: 12, y: 20 } } },
    { seq: 4, kind: 'tool_result', message: '点击已注入，请观察后续画面', data: { call_id: 'c1', tool: 'input_tap', status: 'success', result: { ok: true } } },
    { seq: 5, kind: 'observation', message: '已观察新画面', data: { image_data_url: 'data:image/png;base64,AA==' } },
    { seq: 6, kind: 'reasoning', message: 'NEVER_DISPLAY_PRIVATE_REASONING', data: {} },
  ] }]
  const wrapper = await mountWorkspace()
  expect(wrapper.findAll('.message-user')).toHaveLength(1)
  expect(wrapper.findAll('.message-tool')).toHaveLength(1)
  expect(wrapper.get('.message-tool').text()).toContain('点击已注入')
  expect(wrapper.get('.message-tool pre').text()).toContain('"x": 12')
  expect(wrapper.get('.message-tool pre').text()).toContain('"ok": true')
  expect(wrapper.get('.message-decision').text()).toContain('公开决策说明')
  expect(wrapper.get('.message-user').find('details').exists()).toBe(false)
  expect(wrapper.get('.message-decision').find('details').exists()).toBe(false)
  expect(wrapper.text()).not.toContain('NEVER_DISPLAY_PRIVATE_REASONING')
  expect(wrapper.get('.message-observation details').attributes('open')).toBeUndefined()
})

it('查看其他工作台scope的会话时禁发后续消息，显式新会话绑定当前scope', async () => {
  sessions = [makeSession('paused')]
  const wrapper = await mountWorkspace()
  context.deviceId = 'browser'; context.currentPackageId = 'other'; await flushPromises()
  await wrapper.get('[aria-label="消息"]').setValue('新设备执行目标')
  expect(button(wrapper, '发送并继续').element.disabled).toBe(true)
  await button(wrapper, '新会话').trigger('click')
  await wrapper.get('[aria-label="消息"]').setValue('当前设备观察画面')
  await wrapper.get('#ai-play form').trigger('submit'); await flushPromises()
  const start = mocks.call.mock.calls.find(call => call[1] === 'session.start')
  expect(start[2].device_id).toBe('browser')
  expect(start[2].content_package).toBe('other')
})

it('预算输入校验拒绝不足2048的非零token与小数预算，设为0可继续', async () => {
  sessions = [makeSession('paused')]
  const wrapper = await mountWorkspace()
  await wrapper.get('[aria-controls="ai-budget"]').trigger('click')
  await wrapper.get('[aria-label="消息"]').setValue('继续原任务')
  const tokenBudget = wrapper.get('[aria-label="累计 token 上限（0 表示无上限）"]')
  await tokenBudget.setValue(1024)
  expect(button(wrapper, '继续 AI').element.disabled).toBe(true)
  expect(button(wrapper, '发送并继续').element.disabled).toBe(true)
  expect(button(wrapper, '调整预算并继续').element.disabled).toBe(true)
  await wrapper.get('#ai-play form').trigger('submit'); await flushPromises()
  expect(mocks.call.mock.calls.some(call => call[1] === 'session.message')).toBe(false)
  await tokenBudget.setValue(0)
  expect(button(wrapper, '发送并继续').element.disabled).toBe(false)
  await wrapper.get('[aria-label="最大模型轮数（0 表示无上限）"]').setValue(12.5)
  expect(button(wrapper, '发送并继续').element.disabled).toBe(true)
  expect(wrapper.get('#ai-budget').text()).toContain('运行预算超出允许范围')
})

it('真实tool phase开始显示执行中，结果失败后更新同一张工具卡', async () => {
  sessions = [{ ...makeSession(), events: [{ seq: 1, kind: 'tool', message: '正在执行 input_tap', data: { phase: 'start', generation: 1, call_id: 'c1', tool: 'input_tap', arguments: { x: 4, y: 8 } } }] }]
  const wrapper = await mountWorkspace()
  expect(wrapper.get('.tool-status').text()).toBe('执行中')
  sessions[0].events.push({ seq: 2, kind: 'tool', message: '工具 input_tap 执行失败', data: { phase: 'result', generation: 1, call_id: 'c1', tool: 'input_tap', ok: false, result: { error: 'stale_frame' } } })
  await button(wrapper, '刷新').trigger('click'); await flushPromises()
  expect(wrapper.findAll('.message-tool')).toHaveLength(1)
  expect(wrapper.get('.tool-status').text()).toBe('失败')
  expect(wrapper.get('.message-tool pre').text()).toContain('stale_frame')
})

it('开始会话可将五项预算全部设为0，每项提示无上限并提交完整预算', async () => {
  const wrapper = await mountWorkspace()
  await wrapper.get('[aria-controls="ai-budget"]').trigger('click')
  const fields = wrapper.findAll('[data-budget]')
  expect(fields).toHaveLength(5)
  for (const field of fields) {
    expect(field.attributes('min')).toBe('0')
    expect(field.attributes('aria-label')).toContain('0 表示无上限')
    await field.setValue(0)
  }
  await wrapper.get('[aria-label="消息"]').setValue('不限预算完成当前目标')
  expect(button(wrapper, '发送目标').element.disabled).toBe(false)
  await wrapper.get('#ai-play form').trigger('submit'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'session.start', {
    device_id: 'phone', content_package: 'default', goal: '不限预算完成当前目标', mode: 'api',
    limits: { max_turns: 0, max_actions: 0, max_seconds: 0, max_tokens: 0, max_failures: 0 },
  })
  expect(wrapper.findAll('.usage-grid dd').map(item => item.text())).toEqual(['2 / 不限', '3 / 不限', '4 / 不限', '未知 / 不限', '0 / 不限'])
  expect(wrapper.get('#ai-budget').text()).toContain('用量仍会累计')
})

it('暂停后已有用量超过各上限，五项设0可继续同一run且保留累计用量及未知token下限', async () => {
  const usage = { turns: 101, actions: 201, active_seconds: 601, total_tokens: null, known_tokens: 105396, has_unknown_tokens: true, consecutive_failures: 5 }
  sessions = [{ ...makeSession('paused'), reason: '使用达到预算', usage }]
  const wrapper = await mountWorkspace()
  expect(button(wrapper, '继续 AI').element.disabled).toBe(true)
  await wrapper.get('[aria-controls="ai-budget"]').trigger('click')
  for (const field of wrapper.findAll('[data-budget]')) await field.setValue(0)
  expect(button(wrapper, '继续 AI').element.disabled).toBe(false)
  expect(button(wrapper, '调整预算并继续').element.disabled).toBe(false)
  await button(wrapper, '调整预算并继续').trigger('click'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'session.resume', { session_id: 's1',
    limits: { max_turns: 0, max_actions: 0, max_seconds: 0, max_tokens: 0, max_failures: 0 } })
  expect(mocks.call.mock.calls.some(call => call[1] === 'session.start')).toBe(false)
  expect(sessions[0].run_id).toBe('r1')
  expect(wrapper.findAll('.usage-grid dd').map(item => item.text())).toEqual(['101 / 不限', '201 / 不限', '601 / 不限', '至少 105,396 / 不限', '0 / 不限'])
  expect(wrapper.get('.usage-grid').text()).toContain('部分请求用量未知')
})

it('连续失败达到非零上限仍可明确继续，只有连续失败计数重置', async () => {
  sessions = [{ ...makeSession('paused'), usage: { ...makeSession().usage, consecutive_failures: 3 } }]
  const wrapper = await mountWorkspace()
  expect(button(wrapper, '继续 AI').element.disabled).toBe(false)
  expect(wrapper.get('.usage-grid').text()).toContain('3 / 3')
  await button(wrapper, '继续 AI').trigger('click'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'session.resume', { session_id: 's1' })
  expect(wrapper.findAll('.usage-grid dd').map(item => item.text())).toEqual(['2 / 100', '3 / 200', '4 / 600', '未知 / 100,000', '0 / 3'])
})

it('无上限输入仍拒绝负数、小数、活动时长1至9秒以及超出原范围', async () => {
  const wrapper = await mountWorkspace()
  await wrapper.get('[aria-controls="ai-budget"]').trigger('click')
  await wrapper.get('[aria-label="消息"]').setValue('验证预算输入')
  const defaults = { max_turns: 40, max_actions: 120, max_seconds: 600, max_tokens: 100000, max_failures: 3 }
  const invalid = [
    ...Object.keys(defaults).flatMap(key => [[key, -1], [key, defaults[key] + 0.5]]),
    ...Array.from({ length: 9 }, (_, index) => ['max_seconds', index + 1]),
    ['max_turns', 501], ['max_actions', 2001], ['max_seconds', 7201], ['max_tokens', 2000001], ['max_failures', 21],
  ]
  for (const [key, value] of invalid) {
    const field = wrapper.get(`[data-budget="${key}"]`)
    await field.setValue(value)
    expect(button(wrapper, '发送目标').element.disabled, `${key}=${value}`).toBe(true)
    await wrapper.get('#ai-play form').trigger('submit'); await flushPromises()
    expect(mocks.call.mock.calls.some(call => call[1] === 'session.start'), `${key}=${value}`).toBe(false)
    await field.setValue(defaults[key])
  }
  expect(button(wrapper, '发送目标').element.disabled).toBe(false)
})
