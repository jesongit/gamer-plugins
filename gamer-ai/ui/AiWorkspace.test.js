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
  expect(approved).toMatchObject({ approval_id: 'approval-1', decision: 'approve', scope: 'session', limit: 1 })
  expect(approved.expires_at).toBeGreaterThan(Date.now() / 1000)
  expect(mocks.call.mock.calls.some(c => c[1] === 'sessions.resume')).toBe(false); w.unmount()
})
it('editing an API profile does not repopulate stored credentials', async () => {
  const w = await mounted(); await button(w, '模型与预算').trigger('click'); await flushPromises(); await button(w, '编辑').trigger('click')
  expect(w.get('input[type=password]').element.value).toBe(''); w.unmount()
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
