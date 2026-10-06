// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from 'vitest'
import { effectScope, nextTick, reactive, ref } from 'vue'
import { flushPromises, mount } from '@vue/test-utils'
import { useSourceYamlEditor } from '../../composables/useSourceYamlEditor'
import { allSamplesPassed, sourceDiff, useGenerationState } from './generation-state'
import { parseSampleFile } from './sample-import'
import { safeTraceImageUrl, traceBoxStyle, traceImagesForNode, useRunTrace } from './useRunTrace'
import RunTraceImages from './RunTraceImages.vue'
import AutomationGenerationPanel from './AutomationGenerationPanel.vue'
import SourceYamlEditor from './SourceYamlEditor.vue'
import CandidateTemplatePreview from './CandidateTemplatePreview.vue'
import RunErrorLocation from './RunErrorLocation.vue'
import { api } from '../../../../../../web/src/api'
import { WORKSPACE_CONTEXT_KEY } from '../../../../../../web/src/workspace/context'
import { pluginMessageChannel } from '../../../../../../web/src/workspace/plugin-messages'
const { confirm } = vi.hoisted(() => ({ confirm: Object.assign(vi.fn(async () => true), { cancel: vi.fn() }) }))
vi.mock('../../../../../../web/src/components/ui/useConfirmDialog', () => ({ useConfirmDialog: () => confirm }))
vi.mock('../../../../../../web/src/api', () => ({ api: { callExtension: vi.fn(), listExtensions: vi.fn(), listScripts: vi.fn(), listFunctions: vi.fn(), listPluginResources: vi.fn(), getScript: vi.fn(), updateScript: vi.fn(), createScript: vi.fn() } }))
const pending = () => { let resolve; const promise = new Promise(done => { resolve = done }); return { promise, resolve } }
const candidate = (extra = {}) => ({ id: 'c1', package_id: 'pkg', name: 'demo.yaml', yaml: 'version: 2\nrun: []\n', revision: 1, state: 'draft', sample_ids: ['a', 'b'], report: null, templates: [], attempts: 0, known_tokens: 0, base_version: 'base', ...extra })
const report = (statuses = ['passed', 'passed']) => ({ status: statuses.every(status => status === 'passed') ? 'passed' : 'failed', samples: statuses.map((status, index) => ({ sample_id: ['a', 'b'][index], status, diagnostics: [] })) })
const scopes = []
function session(call) { const scope = effectScope(); scopes.push(scope); const pkg = ref('pkg'); const state = scope.run(() => useGenerationState(pkg, call)); return { pkg, state } }
afterEach(() => { scopes.splice(0).forEach(scope => scope.stop()); vi.restoreAllMocks(); vi.clearAllMocks(); confirm.mockReset().mockResolvedValue(true); vi.useRealTimers() })

describe('source-only YAML v2', () => {
  it('preserves exact comments, targets and future fields without form serialization', async () => {
    const source = '# comment\nversion: 2\ntargets: {button: {template: button.png}}\nrun:\n  - wait: button # observe only\n    then: [{tap: $button}]\n  - finish: done\nfuture_field: keep\n'
    const client = { getScript: vi.fn().mockResolvedValue({ content: source, version: 'v1' }), updateScript: vi.fn().mockResolvedValue({ version: 'v2' }) }
    const editor = useSourceYamlEditor({ api: client, call: vi.fn() }); editor.reset('pkg'); await editor.load('pkg/demo.yaml')
    editor.content.value += '# retained\n'; await editor.save()
    expect(client.updateScript).toHaveBeenCalledWith('pkg/demo.yaml', { content: `${source}# retained\n`, expected_version: 'v1' })
    expect(editor.dirty.value).toBe(false)
  })
  it('discards late loads, saves and validations after a package switch', async () => {
    const read = pending(), save = pending(), validation = pending()
    const client = { getScript: vi.fn().mockReturnValue(read.promise), updateScript: vi.fn().mockReturnValue(save.promise) }
    const editor = useSourceYamlEditor({ api: client, call: vi.fn().mockReturnValue(validation.promise) }); editor.reset('pkg')
    const load = editor.load('pkg/demo.yaml'); editor.reset('other'); read.resolve({ content: 'old', version: 'v1' }); await load; expect(editor.content.value).toBe('')
    editor.reset('pkg'); client.getScript.mockResolvedValue({ content: 'before', version: 'v1' }); await editor.load('pkg/demo.yaml')
    const checking = editor.validate(); validation.resolve({ valid: true, diagnostics: [] }); editor.content.value = 'after'; editor.edited(); await checking; expect(editor.valid.value).toBe(false)
    const saving = editor.save(); editor.reset('other'); save.resolve({ version: 'old-result' }); await saving; expect(editor.version.value).toBe(null); expect(editor.id.value).toBe('')
  })
  it('shows authoritative source diagnostics and refuses implicit force overwrite', async () => {
    const updateScript = vi.fn(), editor = useSourceYamlEditor({ api: { getScript: vi.fn().mockResolvedValue({ content: 'bad' }), updateScript }, call: vi.fn().mockResolvedValue({ valid: false, diagnostics: [{ code: 'yaml.version.required', path: 'version', message: '需要版本 2' }] }) })
    editor.reset('pkg'); await editor.load('pkg/demo.yaml'); expect(await editor.validate()).toBe(false); expect(editor.diagnostics.value[0].path).toBe('version')
    expect(await editor.save()).toBe(false); expect(updateScript).not.toHaveBeenCalled(); expect(editor.error.value).toContain('版本缺失')
  })
  it('allows no-device edit and syntax validation while AI and live run are disabled', async () => {
    api.listScripts.mockResolvedValue([{ id: 'pkg/demo.yaml', name: 'demo.yaml' }]); api.listExtensions.mockResolvedValue([])
    api.getScript.mockResolvedValue({ content: 'version: 2\nrun: []\n', version: 'v1' }); api.callExtension.mockResolvedValue({ valid: true, diagnostics: [] })
    const context = { runKind: 'script', packageId: 'pkg', store: { deviceId: '', running: false }, runScript: vi.fn() }
    const wrapper = mount(SourceYamlEditor, { props: { context }, global: { stubs: { RunDetails: true, RunErrorLocation: true } } }); await flushPromises()
    try {
      expect(wrapper.get('textarea').element.value).toContain('version: 2')
      const button = label => wrapper.findAll('button').find(item => item.text() === label)
      expect(button('运行').attributes('disabled')).toBeDefined(); expect(button('交给 AI 分析').attributes('disabled')).toBeDefined()
      await button('校验源码').trigger('click'); await flushPromises(); expect(wrapper.text()).toContain('源码校验通过')
      expect(api.callExtension).toHaveBeenCalledWith('gamer-yaml', 'automation.validate_source', { package_id: 'pkg', yaml: 'version: 2\nrun: []\n', kind: 'script' })
    } finally { wrapper.unmount() }
  })
})

describe('candidate revisions and validation gate', () => {
  it('requires every exact selected sample, rejects duplicates and evidence insufficiency', () => {
    expect(allSamplesPassed(candidate({ state: 'passed', report: report() }))).toBe(true)
    for (const reportValue of [report(['passed', 'insufficient_evidence']), { status: 'passed', samples: [{ sample_id: 'a', status: 'passed' }] }, { status: 'passed', samples: [{ sample_id: 'a', status: 'passed' }, { sample_id: 'a', status: 'passed' }] }]) expect(allSamplesPassed(candidate({ state: 'passed', report: reportValue }))).toBe(false)
  })
  it('keeps late generation results in their original package and never writes new context', async () => {
    const start = pending(), call = vi.fn((action) => action === 'generation.start' ? start.promise : Promise.resolve({ candidates: [], ready: false, revisions: [] }))
    const { state, pkg } = session(call); await flushPromises()
    const result = state.operation('generation.start', { name: 'demo.yaml' }, { useCandidate: false }); pkg.value = 'other'; start.resolve({ candidate: candidate() }); await result
    expect(state.candidate.value).toBe(null); expect(state.busy.value).toBe(false); expect(call).not.toHaveBeenCalledWith('generation.save', expect.anything())
  })
  it('dirty source invalidates passing report and final save, drafts stay explicit', async () => {
    const call = vi.fn(async action => action === 'generation.get' ? { candidate: candidate({ state: 'passed', report: report() }), base_yaml: '', base_exists: false } : { ready: false, candidates: [] })
    const { state } = session(call); await state.select('c1'); expect(state.canSave.value).toBe(true)
    state.yaml.value += '# edit'; expect(state.canSave.value).toBe(false); expect(await state.save('validated')).toBe(false)
    expect(call.mock.calls.some(([action]) => action === 'generation.save')).toBe(false)
  })
  it('ignores out-of-order candidate selections and stale poll after cancel', async () => {
    vi.useFakeTimers(); const first = pending(), second = pending(), poll = pending(); let reads = 0
    const call = vi.fn(action => action === 'generation.get' ? [first.promise, second.promise, poll.promise][reads++] : action === 'generation.cancel' ? Promise.resolve({ candidate: candidate({ state: 'cancelled', revision: 3 }) }) : Promise.resolve({ ready: true, candidates: [] }))
    const { state } = session(call); const one = state.select('old'), two = state.select('c1'); second.resolve({ candidate: candidate({ state: 'generating', revision: 2 }) }); await two; first.resolve({ candidate: candidate({ id: 'old' }) }); await one
    expect(state.candidate.value.id).toBe('c1'); await vi.advanceTimersByTimeAsync(700)
    await state.operation('generation.cancel'); poll.resolve({ candidate: candidate({ state: 'passed', revision: 2, report: report() }) }); await flushPromises()
    expect(state.candidate.value.state).toBe('cancelled'); expect(state.canSave.value).toBe(false)
  })
  it('renders meaningful optional AI unavailability and manual mode without a device', async () => {
    api.callExtension.mockImplementation(async (_, action) => action === 'generation.readiness' ? { ready: false, reason: 'AI 插件未安装' } : action === 'generation.list' ? { candidates: [] } : { samples: [], revisions: [] })
    api.listPluginResources.mockResolvedValue({ resources: [] })
    const wrapper = mount(AutomationGenerationPanel, { props: { packageId: 'pkg' } }); await flushPromises()
    try { expect(wrapper.text()).toContain('AI 插件未安装'); expect(wrapper.text()).toContain('手写源码离线验证'); expect(wrapper.findAll('button').find(button => button.text() === '开始生成并验证全部素材').attributes('disabled')).toBeDefined() } finally { wrapper.unmount() }
  })
  it('uses the shared AI model, opens its settings and refreshes after configuration changes', async () => {
    let ready = false
    api.callExtension.mockImplementation(async (_, action) => action === 'generation.readiness'
      ? { ready, reason: ready ? null : 'model_not_configured', model: { model: 'shared-vision', default_limits: { max_turns: 40, max_seconds: 600, max_tokens: 100000, max_failures: 3 } } }
      : { candidates: [], revisions: [], samples: [] })
    api.listPluginResources.mockResolvedValue({ resources: [] })
    const openPanel = vi.fn(), request = pluginMessageChannel('gamer-ai:open-settings'), before = request.seq
    const wrapper = mount(AutomationGenerationPanel, { props: { packageId: 'pkg' }, global: { provide: { [WORKSPACE_CONTEXT_KEY]: { uiBridge: { workspace: { openPanel } } } } } }); await flushPromises()
    try {
      expect(wrapper.text()).toContain('AI 助手尚未保存 API 密钥')
      expect(wrapper.text()).not.toContain('model_not_configured')
      expect(wrapper.text()).toContain('shared-vision')
      expect(wrapper.text()).toContain('100000 Token')
      expect(wrapper.findAll('input[type="number"]')).toHaveLength(0)
      await wrapper.findAll('button').find(button => button.text() === 'AI 助手模型设置').trigger('click')
      expect(openPanel).toHaveBeenCalledWith('gamer-ai:ai'); expect(request.seq).toBe(before + 1)
      ready = true; pluginMessageChannel('gamer-ai:settings-changed').seq++; await flushPromises()
      expect(wrapper.text()).toContain('图片识别能力测试通过')
    } finally { wrapper.unmount() }
  })
  it('lets the server inherit AI defaults instead of sending an independent generation budget', async () => {
    api.callExtension.mockImplementation(async (_, action) => action === 'generation.readiness' ? { ready: true }
      : action === 'sample.list' ? { samples: [{ id: 'demo', name: 'Demo' }] }
      : { candidates: [], revisions: [] })
    api.listPluginResources.mockResolvedValue({ resources: [] })
    const wrapper = mount(AutomationGenerationPanel, { props: { packageId: 'pkg' } }); await flushPromises()
    try {
      await wrapper.get('[aria-label="生成目标"]').setValue('Done is visible')
      await wrapper.get('.samples input[type="checkbox"]').setValue(true)
      await wrapper.findAll('button').find(button => button.text() === '开始生成并验证全部素材').trigger('click'); await flushPromises()
      expect(api.callExtension).toHaveBeenCalledWith('gamer-yaml', 'generation.start', {
        package_id: 'pkg', name: 'automation.yaml', goal: 'Done is visible', samples: [{ sample_id: 'demo', plugin_id: 'gamer-video' }],
      })
    } finally { wrapper.unmount() }
  })
  it('produces an honest source diff with changed lines', () => { expect(sourceDiff('a\nb\nc', 'a\nd\nc')).toBe('@@ 第 2 行 @@\n- b\n+ d'); expect(sourceDiff('a', 'a')).toBe('没有源码变化') })
})

describe('trace evidence and imported samples', () => {
  it('rejects missing assets and traversal before handing imported JSON to backend', () => {
    expect(() => parseSampleFile('{"manifest":{},"files":[]}')).toThrow('缺少图片')
    expect(() => parseSampleFile(JSON.stringify({ manifest: {}, files: [{ path: '../secret', base64: 'AAAA' }] }))).toThrow('不安全')
  })
  it('does not confuse call frame IDs with image identity and uses only scoped image URLs', () => {
    const images = [{ image_id: 'image-3', metadata: { frame_id: 9, path: 'run[1]' }, url: 'https://evil.invalid/track' }, { image_id: '9', metadata: { frame_id: 1 } }]
    expect(traceImagesForNode(images, { trace: { frame_id: 9 } })).toEqual([images[0]])
    expect(safeTraceImageUrl('run/a', images[0])).toBe('/api/runs/run%2Fa/trace/images/image-3')
    expect(traceBoxStyle([10, 20, 30, 40], 100, 200)).toEqual({ left: '10%', top: '10%', width: '30%', height: '20%' })
  })
  it('clears expired assets and discards old-run reads', async () => {
    const request = pending(), run = ref('old'), visible = ref(true), client = { getRunTrace: vi.fn().mockReturnValueOnce(request.promise).mockResolvedValue({ status: 'expired', images: [], gaps: [], next: 0 }) }
    const scope = effectScope(); scopes.push(scope); const trace = scope.run(() => useRunTrace(run, visible, client)); run.value = 'new'; await flushPromises()
    request.resolve({ status: 'available', images: [{ image_id: 'old' }] }); await flushPromises(); expect(trace.status.value).toBe('expired'); expect(trace.images.value).toEqual([])
  })
  it('shows broken, expired and gap evidence rather than substituting another screenshot', async () => {
    const wrapper = mount(RunTraceImages, { props: { runId: 'run', status: 'available', enabled: false, gaps: [{ reason: 'quota', count: 1 }], images: [{ image_id: 'a', kind: 'error_last_consumed', seq: 1, width: 100, height: 100, metadata: {} }] } })
    try { expect(wrapper.text()).toContain('Trace 已关闭'); expect(wrapper.text()).toContain('证据缺口'); await wrapper.get('img').trigger('error'); expect(wrapper.text()).toContain('图像不可用或已过期'); await wrapper.setProps({ status: 'expired', images: [] }); expect(wrapper.text()).toContain('图像证据已过期') } finally { wrapper.unmount() }
  })
})


describe('human candidate approval boundaries', () => {
  function prepare(current) {
    api.callExtension.mockImplementation(async (_, action) => {
      if (action === 'generation.readiness') return { ready: true }
      if (action === 'generation.list') return { candidates: [current] }
      if (action === 'generation.get') return { candidate: current, base_yaml: '', base_exists: false }
      if (action === 'generation.apply_proposal') return { candidate: { ...current, yaml: current.pending_proposal.proposal.yaml, revision: current.revision + 1, state: 'draft', report: null, pending_proposal: null } }
      return { samples: [], resources: [], revisions: [] }
    })
    api.listPluginResources.mockResolvedValue({ resources: [] })
  }
  it('pending proposal stays read-only until explicit approval, then invalidates previous report', async () => {
    prepare(candidate({ state: 'passed', report: report(), pending_proposal: { id: 'proposal-1', base_revision: 1, proposal: { yaml: 'version: 2\n# suggested\nrun: []', templates: [], explanation: '检查完成目标' } } }))
    const wrapper = mount(AutomationGenerationPanel, { props: { packageId: 'pkg' } }); await flushPromises()
    try {
      await wrapper.get('.candidate-picker select').setValue('c1'); await flushPromises()
      expect(wrapper.get('[aria-label="候选 YAML"]').element.value).not.toContain('suggested')
      expect(wrapper.text()).toContain('尚未修改当前源码')
      expect(api.callExtension.mock.calls.some(([,action]) => action === 'generation.apply_proposal')).toBe(false)
      await wrapper.findAll('button').find(button => button.text() === '审核并应用到候选').trigger('click'); await flushPromises()
      expect(api.callExtension).toHaveBeenCalledWith('gamer-yaml', 'generation.apply_proposal', { package_id: 'pkg', candidate_id: 'c1', expected_revision: 1, proposal_id: 'proposal-1' })
      expect(wrapper.get('[aria-label="候选 YAML"]').element.value).toContain('suggested')
      expect(wrapper.findAll('button').find(button => button.text() === '确认保存正式版本').element.disabled).toBe(true)
      expect(api.callExtension.mock.calls.some(([,action]) => action === 'generation.save')).toBe(false)
    } finally { wrapper.unmount() }
  })
  it('an old confirmation cannot save a candidate after switching package', async () => {
    prepare(candidate({ state: 'passed', report: report() })); const approval = pending(); confirm.mockReturnValueOnce(approval.promise)
    const wrapper = mount(AutomationGenerationPanel, { props: { packageId: 'pkg' } }); await flushPromises()
    try {
      await wrapper.get('.candidate-picker select').setValue('c1'); await flushPromises()
      await wrapper.findAll('button').find(button => button.text() === '确认保存正式版本').trigger('click')
      await wrapper.setProps({ packageId: 'other' }); approval.resolve(true); await flushPromises()
      expect(api.callExtension.mock.calls.some(([,action]) => action === 'generation.save')).toBe(false)
    } finally { wrapper.unmount() }
  })
  it('template preview rejects wrong identities and stale revisions', async () => {
    const request = pending(); api.callExtension.mockReturnValueOnce(request.promise)
    const wrapper = mount(CandidateTemplatePreview, { props: { packageId: 'pkg', candidateId: 'c1', revision: 1, name: 'button.png' } })
    try {
      await wrapper.get('button').trigger('click'); await wrapper.setProps({ revision: 2 })
      request.resolve({ name: 'button.png', mime_type: 'image/png', base64: 'AAAA', width: 2, height: 2 }); await flushPromises()
      expect(wrapper.find('img').exists()).toBe(false)
      api.callExtension.mockResolvedValueOnce({ name: 'other.png', mime_type: 'image/png', base64: 'AAAA', width: 2, height: 2 })
      await wrapper.get('button').trigger('click'); await flushPromises()
      expect(wrapper.get('[role="alert"]').text()).toContain('身份不匹配'); expect(wrapper.find('img').exists()).toBe(false)
    } finally { wrapper.unmount() }
  })
})


it('source location uses verified frozen v2 source after the current file changes', async () => {
  const source = { package_id: 'pkg', path: 'automations/daily.yaml', version: 'old' }
  const yaml = '# immutable evidence\nversion: 2\nrun: [{finish: {template: done.png}}]'
  const wrapper = mount(RunErrorLocation, { props: { event: { path: 'run[0]', trace: { run_id: 'run1', frame_id: 7, source }, source_snapshot: { trace: { run_id: 'run1', entry: source, functions: {} }, _source_files: { 'automations/daily.yaml': yaml } } } }, global: { stubs: { Teleport: true } } })
  try { await flushPromises(); expect(wrapper.get('[aria-label="运行版本源码"]').text()).toBe(yaml); expect(wrapper.text()).toContain('执行时冻结版本'); expect(api.getScript).not.toHaveBeenCalled() } finally { wrapper.unmount() }
})


it('a late resource selection cannot change the new package selection', async () => {
  const old = pending()
  api.listExtensions.mockResolvedValue([])
  api.listScripts.mockImplementation(async pkg => pkg === 'pkg' ? [{ id: 'pkg/one.yaml', name: 'one.yaml' }, { id: 'pkg/two.yaml', name: 'two.yaml' }] : [{ id: 'other/new.yaml', name: 'new.yaml' }])
  api.getScript.mockImplementation(id => id === 'pkg/two.yaml' ? old.promise : Promise.resolve({ id, content: 'version: 2\nrun: []', version: id }))
  const context = reactive({ packageId: 'pkg', runKind: 'script', store: { running: false, deviceId: '' }, selScript: '' })
  const wrapper = mount(SourceYamlEditor, { props: { context }, global: { stubs: { RunDetails: true, RunErrorLocation: true } } }); await flushPromises()
  try {
    await wrapper.get('[aria-label="选择源码文件"]').setValue('pkg/two.yaml'); await flushPromises()
    context.packageId = 'other'; await flushPromises()
    old.resolve({ id: 'pkg/two.yaml', content: 'old source', version: 'old' }); await flushPromises()
    expect(context.selScript).toBe('other/new.yaml'); expect(wrapper.get('[aria-label="选择源码文件"]').element.value).toBe('other/new.yaml'); expect(wrapper.get('textarea').element.value).not.toBe('old source')
  } finally { wrapper.unmount() }
})


it('final review shows exact tested arguments, frozen execution settings and default-argument warning', async () => {
  const current = candidate({ state: 'passed', report: report(), execution_settings: { default_timeout_secs: 999, before_click_ms: 999, after_click_ms: 999 } })
  const scope = { tested_args: { retries: 2, popup: false }, default_args_match: false, validation_status: 'passed', other_args_verified: false, execution_settings: { default_timeout_secs: 17, before_click_ms: 0, after_click_ms: 350 }, samples: [{ id: 'a', content_sha256: 'aaa111' }, { id: 'b', content_sha256: 'bbb222' }] }
  api.callExtension.mockImplementation(async (_, action) => action === 'generation.get' ? { candidate: current, base_yaml: '', base_exists: false, verification_scope: scope } : action === 'generation.list' ? { candidates: [current] } : action === 'generation.readiness' ? { ready: true } : { samples: [], revisions: [] })
  api.listPluginResources.mockResolvedValue({ resources: [] })
  const wrapper = mount(AutomationGenerationPanel, { props: { packageId: 'pkg' } }); await flushPromises()
  try {
    await wrapper.get('.candidate-picker select').setValue('c1'); await flushPromises()
    const review = wrapper.get('[aria-label="验证范围与冻结设置"]')
    expect(JSON.parse(review.get('[aria-label="受测参数"]').text())).toEqual({ retries: 2, popup: false })
    expect(review.text()).toContain('默认参数未验证')
    const settings = review.get('[aria-label="受测执行设置"]').text()
    expect(settings).toContain('默认超时 17 秒'); expect(settings).toContain('点击前延迟 0 ms'); expect(settings).toContain('点击后延迟 350 ms'); expect(settings).not.toContain('999')
    expect(review.findAll('input,select,textarea')).toHaveLength(0)
    expect(review.get('details').element.open).toBe(false)
    review.get('details').element.open = true; await review.get('details').trigger('toggle')
    expect(review.get('details').text()).toContain('aaa111'); expect(review.get('details').text()).toContain('bbb222')
    await wrapper.get('[aria-label="候选 YAML"]').setValue(`${current.yaml}# changed`)
    expect(review.text()).toContain('当前没有有效的全部通过报告')
    await wrapper.setProps({ packageId: 'other' }); await flushPromises()
    expect(wrapper.find('[aria-label="验证范围与冻结设置"]').exists()).toBe(false)
  } finally { wrapper.unmount() }
})


it('saves the selected original trace PNG via its authenticated same-origin URL without rewriting pixels', async () => {
  const first = { image_id: 'image-a', kind: 'consumed', width: 100, height: 100, metadata: {}, url: 'https://untrusted.invalid/export' }
  const second = { ...first, image_id: 'image-b', kind: 'error_fresh' }
  const wrapper = mount(RunTraceImages, { props: { runId: 'run/a', status: 'available', enabled: true, images: [first, second] } })
  try {
    const link = wrapper.get('a[aria-label="保存原始画面"]')
    expect(link.attributes('href')).toBe('/api/runs/run%2Fa/trace/images/image-a')
    expect(link.attributes('download')).toBe('trace-run_a-image-a.png')
    expect(link.attributes('href')).not.toContain('untrusted.invalid')
    expect(wrapper.find('canvas').exists()).toBe(false)
    expect(wrapper.text()).toContain('不是完整回放素材')
    await wrapper.findAll('.thumbnail')[1].trigger('click')
    expect(wrapper.get('a[aria-label="保存原始画面"]').attributes('href')).toBe('/api/runs/run%2Fa/trace/images/image-b')
    expect(wrapper.get('a[aria-label="保存原始画面"]').attributes('download')).toBe('trace-run_a-image-b.png')
    expect(api.callExtension).not.toHaveBeenCalled()
  } finally { wrapper.unmount() }
})

it('does not offer a download link for expired or unreadable trace evidence', async () => {
  const item = { image_id: 'image-a', kind: 'consumed', width: 100, height: 100, metadata: {} }
  const wrapper = mount(RunTraceImages, { props: { runId: 'run', status: 'available', enabled: true, images: [item] } })
  try {
    await wrapper.get('.thumbnail img').trigger('error')
    expect(wrapper.find('a[download]').exists()).toBe(false)
    expect(wrapper.get('button[aria-label="保存原始画面"]').element.disabled).toBe(true)
    await wrapper.setProps({ runId: 'new-run', status: 'expired', images: [{ ...item, image_id: 'expired-image' }] })
    expect(wrapper.find('a[download]').exists()).toBe(false)
    expect(wrapper.get('button[aria-label="保存原始画面"]').element.disabled).toBe(true)
  } finally { wrapper.unmount() }
})
