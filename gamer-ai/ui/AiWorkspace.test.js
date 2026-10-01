import { beforeEach, expect, it, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import { WORKSPACE_CONTEXT_KEY } from '../../../web/src/workspace/context'
const mocks = vi.hoisted(() => ({ call: vi.fn(), run: vi.fn() }))
vi.mock('../../../web/src/api', () => ({ api: { callExtension: mocks.call, run: mocks.run } }))
import AiWorkspace from './AiWorkspace.vue'
const ctx = { deviceId: 'device-1', androidPackageName: 'com.android.game', currentPackageId: 'daily-data' }
const settings = { version: 'v1', profiles: [{ id: 'vision', model: 'vl', protocol: 'ollama', endpoint: 'http://localhost:11434', has_key: true, vision: 'available' }], budget: { max_rounds: 40 }, notify_results: true, search: null }
const session = { id: 'session-1', goal: '日常目标', app: 'com.android.game', package: 'daily-data', state: 'running', runs: ['run-1'], progress: [] }
beforeEach(() => {
  mocks.run.mockReset().mockResolvedValue({ run_id: 'run-2' })
  mocks.call.mockReset().mockImplementation(async (_, action) => ({
    'settings.read': settings, 'sessions.read': { sessions: [session] }, 'sessions.events': { events: [], state: 'running' },
    'approvals.read': { approvals: [{ id: 'approval-1', session: 'session-1', status: 'pending', consumption: { resource: '门票', quantity: 1, purpose: '挑战', evidence: '按钮显示 1 张' } }], rules: [] },
    'credentials.read': { credentials: [] }, 'usage.read': { requests: [] },
    'settings.discover': { endpoint: 'https://models.example/v1', protocol: 'chat', models: [{ id: 'vision-model', image_input: true }], vision: 'untested' },
    'settings.save': { ...settings, version: 'v2' },
  }[action] || {}))
})
const mounted = async () => { const w = mount(AiWorkspace, { global: { provide: { [WORKSPACE_CONTEXT_KEY]: { getSnapshot: () => ctx } } } }); await flushPromises(); return w }
const button = (w, name) => w.findAll('button').find(b => b.text() === name)
it('uses separate Android and Package contexts and never starts paid calls on mount', async () => {
  const w = await mounted()
  expect(mocks.run).not.toHaveBeenCalled()
  expect(mocks.call.mock.calls.some(c => c[1] === 'settings.test')).toBe(false)
  await w.get('textarea').setValue('完成日常')
  await w.get('form').trigger('submit'); await flushPromises()
  expect(mocks.run).toHaveBeenCalledWith({ runner_id: 'gamer-ai', entrypoint: 'daily-data#goal', device_id: 'device-1', content_package: 'daily-data', payload: { goal: '完成日常', model_profile_id: '', resume_session_id: null } })
  w.unmount()
})
it('cancel and takeover use host actions without disabling the plugin', async () => {
  const w = await mounted(); await button(w, '停止').trigger('click'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'sessions.cancel', { session_id: 'session-1' })
  await button(w, '人工接管').trigger('click'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'input.takeover', { device_id: 'device-1' })
  w.unmount(); expect(mocks.call.mock.calls.some(c => c[1] === 'disable')).toBe(false)
})
it('approval submits explicit scope, total limit and expiry and does not auto-resume', async () => {
  const w = await mounted(); await w.findAll('button').find(b => b.text().startsWith('日常目标')).trigger('click'); await flushPromises()
  await button(w, '授权').trigger('click'); await flushPromises()
  await button(w, '批准所选范围与额度').trigger('click'); await flushPromises()
  const approved = mocks.call.mock.calls.find(c => c[1] === 'approvals.resolve')[2]
  expect(approved).toMatchObject({ approval_id: 'approval-1', decision: 'approve', scope: 'operation', limit: 1 })
  expect(approved.expires_at).toBeGreaterThan(Date.now() / 1000)
  expect(mocks.call.mock.calls.some(c => c[1] === 'sessions.resume')).toBe(false); w.unmount()
})
it('editing an API profile does not repopulate stored credentials', async () => {
  const w = await mounted(); await button(w, '模型与预算').trigger('click'); await flushPromises(); await button(w, '编辑').trigger('click')
  expect(w.get('input[type=password]').element.value).toBe(''); w.unmount()
})
it('initial model setup exposes only URL and key, discovers and saves without inference', async () => {
  const w = await mounted(); await button(w, '模型与预算').trigger('click')
  const form = w.get('.model-form')
  const basic = form.findAll('input, select').filter(input => !input.element.closest('details'))
  expect(basic.map(input => input.attributes('type'))).toEqual(['url', 'password'])
  expect(w.get('.model-advanced').element.open).toBe(false)
  expect(w.get('.budget-advanced').element.open).toBe(false)
  await basic[0].setValue('https://models.example/v1'); await basic[1].setValue('private-key')
  await form.trigger('submit'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'settings.discover', { endpoint: 'https://models.example/v1', key: 'private-key', protocol: 'auto', profile_id: 'primary' })
  const saved = mocks.call.mock.calls.find(c => c[1] === 'settings.save')[2]
  expect(saved.expected_version).toBe('v1')
  expect(saved.settings.profiles.find(p => p.id === 'primary')).toMatchObject({ protocol: 'chat', model: 'vision-model', vision: 'untested', key: 'private-key', price_version: '' })
  expect(basic[1].element.value).toBe('')
  expect(mocks.call.mock.calls.some(c => c[1] === 'settings.test')).toBe(false)
  expect(mocks.run).not.toHaveBeenCalled(); w.unmount()
})
it('failed discovery keeps the draft and opens manual model settings without saving', async () => {
  const previous = mocks.call.getMockImplementation()
  mocks.call.mockImplementation(async (...args) => { if (args[1] === 'settings.discover') throw Error('模型列表 HTTP 404'); return previous(...args) })
  const w = await mounted(); await button(w, '模型与预算').trigger('click')
  await w.get('input[type=url]').setValue('https://custom.example/v1')
  await w.get('.model-form').trigger('submit'); await flushPromises()
  expect(w.get('.model-advanced').element.open).toBe(true)
  expect(w.get('input[type=url]').element.value).toBe('https://custom.example/v1')
  expect(w.get('[role=alert]').text()).toContain('HTTP 404')
  expect(mocks.call.mock.calls.some(c => c[1] === 'settings.save')).toBe(false); w.unmount()
})
it('manual setup supports services without a model list and preserves unrelated settings', async () => {
  const w = await mounted(); await button(w, '模型与预算').trigger('click')
  await w.get('input[type=url]').setValue('http://127.0.0.1:11434')
  await w.get('.model-advanced select').setValue('ollama')
  await w.get('input[list=ai-model-options]').setValue('local-vision')
  await w.get('.model-form').trigger('submit'); await flushPromises()
  expect(mocks.call.mock.calls.some(c => c[1] === 'settings.discover')).toBe(false)
  const saved = mocks.call.mock.calls.find(c => c[1] === 'settings.save')[2].settings
  expect(saved.profiles.find(p => p.id === 'primary')).toMatchObject({ protocol: 'ollama', model: 'local-vision', key: '' })
  expect(saved.profiles.find(p => p.id === 'vision')).toMatchObject({ model: 'vl', key: '' })
  expect(saved.budget).toEqual(settings.budget); expect(saved.notify_results).toBe(true); w.unmount()
})
it('guide edits use the current Package and optimistic version without rendering HTML', async () => {
  const original = mocks.call.getMockImplementation()
  mocks.call.mockImplementation(async (...args) => {
    if (args[1] === 'memory.search') return { resources: [{ path: 'guides/imported.json' }] }
    if (args[1] === 'memory.read') return { resource: { content: '{"content":"<script>untrusted</script>"}', version: 'guide-v1' }, memory: { effective_status: 'candidate' } }
    return original(...args)
  })
  const w = await mounted(); await button(w, '攻略与方案').trigger('click')
  await button(w, '读取当前包攻略').trigger('click'); await flushPromises()
  await button(w, '读取和编辑').trigger('click'); await flushPromises()
  expect(w.find('script').exists()).toBe(false)
  await w.get('textarea').setValue('{"content":"修订攻略"}')
  await button(w, '保存修订').trigger('click'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'memory.update', { package_id: 'daily-data', path: 'guides/imported.json', memory: { content: '修订攻略' }, expected_version: 'guide-v1' })
  w.unmount()
})

it('shows plan, decisions and tool outcomes alongside the conversation', async () => {
  const previous = mocks.call.getMockImplementation()
  mocks.call.mockImplementation(async (...args) => args[1] === 'sessions.events' ? { questions: [], events: args[2].after ? [] : [
    { seq: 1, kind: 'thinking', data: { summary: '正在核对当前画面和攻略' } },
    { seq: 2, kind: 'plan', data: { summary: '已查到活动入口，先检查前置', plan: { steps: [{ description: '进入活动面板', expected: '完成状态可见' }] } } },
    { seq: 3, kind: 'decision', data: { summary: '<img src=x onerror=alert(1)>继续检查' } },
    { seq: 4, kind: 'tool_result', data: { tool: 'act', result: { status: 'injected' } } },
  ] } : previous(...args))
  const w = await mounted(); await button(w, '日常目标 · 执行中').trigger('click'); await flushPromises()
  expect(w.get('[role=log]').text()).toContain('进入活动面板 · 验证：完成状态可见')
  expect(w.get('[role=log]').text()).toContain('接下来用新画面核对结果')
  expect(w.get('[role=log]').find('img').exists()).toBe(false)
  await w.get('.chat-composer textarea').setValue('先检查前置，不要进入挑战')
  await w.get('.chat-composer').trigger('submit'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'sessions.message', { session_id: 'session-1', message: '先检查前置，不要进入挑战' })
  expect(mocks.call.mock.calls.some(c => c[1] === 'sessions.resume')).toBe(false)
  w.unmount()
})

it('question choice sends a real answer and resumes only after the checkpoint', async () => {
  const previous = mocks.call.getMockImplementation(); let answered = false
  mocks.call.mockImplementation(async (...args) => {
    if (args[1] === 'sessions.read') return { sessions: [{ ...session, state: 'waiting_user' }] }
    if (args[1] === 'approvals.read') return { approvals: [], rules: [] }
    if (args[1] === 'sessions.message') { answered = true; return { accepted: true } }
    if (args[1] === 'sessions.events') return { events: [], questions: answered ? [] : [{ id: 'question-1', question: '活动入口在哪里？', options: ['侧边活动页', '先完成前置'], reason: '攻略缺少入口', kind: 'knowledge', deadline: Math.floor(Date.now() / 1000) + 120, answer: null, timed_out: false }] }
    return previous(...args)
  })
  const w = await mounted(); await button(w, '日常目标 · 等待你的回答或授权').trigger('click'); await flushPromises()
  expect(w.get('.question-card').text()).toContain('等待剩余')
  await button(w, '侧边活动页').trigger('click'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'sessions.message', { session_id: 'session-1', question_id: 'question-1', message: '侧边活动页' })
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'sessions.resume', { session_id: 'session-1', device_id: 'device-1', model_profile_id: '' })
  expect(mocks.call.mock.calls.some(c => c[1] === 'approvals.resolve')).toBe(false)
  w.unmount()
})

it('permanent permission requires the explicit card action and supports no expiry', async () => {
  const previous = mocks.call.getMockImplementation(); let approved = false
  mocks.call.mockImplementation(async (...args) => {
    if (args[1] === 'sessions.read') return { sessions: [{ ...session, state: 'waiting_user', account: 'confirmed-account' }] }
    if (args[1] === 'approvals.resolve') { approved = true; return { resolved: true } }
    if (args[1] === 'approvals.read') { const result = await previous(...args); return { ...result, approvals: result.approvals.map(a => ({ ...a, status: approved ? 'approved' : 'pending' })) } }
    return previous(...args)
  })
  const w = await mounted(); await button(w, '日常目标 · 等待你的回答或授权').trigger('click'); await flushPromises()
  await w.get('.approval-card select').setValue('persistent')
  expect(w.get('.approval-card input[type=checkbox]').element.checked).toBe(true)
  await w.get('.chat-composer textarea').setValue('我同意永久授权')
  await w.get('.chat-composer').trigger('submit'); await flushPromises()
  expect(mocks.call.mock.calls.some(c => c[1] === 'approvals.resolve')).toBe(false)
  expect(mocks.call.mock.calls.some(c => c[1] === 'sessions.resume')).toBe(false)
  await button(w, '确认授权并继续').trigger('click'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'approvals.resolve', { approval_id: 'approval-1', decision: 'approve', scope: 'persistent', limit: 1, no_expiry: true })
  expect(mocks.call.mock.calls.some(c => c[1] === 'sessions.resume')).toBe(true)
  w.unmount()
})

it('Chinese composition and Shift Enter do not submit a goal', async () => {
  const w = await mounted(); const input = w.get('.chat-composer textarea')
  await input.setValue('完成活动')
  await input.trigger('keydown', { key: 'Enter', isComposing: true }); await flushPromises()
  await input.trigger('keydown', { key: 'Enter', shiftKey: true }); await flushPromises()
  expect(mocks.run).not.toHaveBeenCalled()
  await input.trigger('keydown', { key: 'Enter' }); await flushPromises()
  expect(mocks.run).toHaveBeenCalledTimes(1)
  w.unmount()
})

it('historical account reference can be corrected explicitly without approving consumption', async () => {
  const previous = mocks.call.getMockImplementation()
  mocks.call.mockImplementation(async (...args) => args[1] === 'sessions.read' ? { sessions: [{ ...session, state: 'partial', account: 'previous-account', account_reference_only: true }] } : previous(...args))
  const w = await mounted(); await button(w, '日常目标 · 部分完成').trigger('click'); await flushPromises()
  expect(w.get('.identity-reference').text()).toContain('仅供参考')
  await w.get('.identity-reference input').setValue('verified-current-account')
  await w.get('.identity-reference').trigger('submit'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai', 'identity.confirm', { session_id: 'session-1', account: 'verified-current-account', cycle: '' })
  expect(mocks.call.mock.calls.some(c => c[1] === 'approvals.resolve')).toBe(false)
  w.unmount()
})
