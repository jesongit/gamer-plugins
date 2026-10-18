// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from 'vitest'
import { effectScope, nextTick, reactive, ref } from 'vue'
import { flushPromises, mount } from '@vue/test-utils'
import { useSourceYamlEditor } from '../../composables/useSourceYamlEditor'
import { sourceDiff } from './generation-state'
import { parseSampleFile } from './sample-import'
import { safeTraceImageUrl, traceBoxStyle, traceImagesForNode, useRunTrace } from './useRunTrace'
import RunTraceImages from './RunTraceImages.vue'
import AutomationGenerationPanel from './AutomationCandidateReview.vue'
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
const candidate = (extra = {}) => ({ id: 'c1', package_id: 'pkg', name: 'demo.yaml', yaml: 'run: []\n', revision: 1, state: 'draft', sample_ids: ['a', 'b'], report: null, templates: [], attempts: 0, known_tokens: 0, base_version: 'base', ...extra })
const report = (statuses = ['passed', 'passed']) => ({ status: statuses.every(status => status === 'passed') ? 'passed' : 'failed', samples: statuses.map((status, index) => ({ sample_id: ['a', 'b'][index], status, diagnostics: [] })) })
const scopes = []
afterEach(() => { scopes.splice(0).forEach(scope => scope.stop()); vi.restoreAllMocks(); vi.clearAllMocks(); confirm.mockReset().mockResolvedValue(true); vi.useRealTimers() })

describe('source-only YAML', () => {
  it('preserves exact comments, targets and future fields without form serialization', async () => {
    const source = '# comment\ntargets: {button: {template: button.png}}\nrun:\n  - wait: button # observe only\n    then: [{tap: $button}]\n  - finish: done\nfuture_field: keep\n'
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
    const updateScript = vi.fn(), editor = useSourceYamlEditor({ api: { getScript: vi.fn().mockResolvedValue({ content: 'bad' }), updateScript }, call: vi.fn().mockResolvedValue({ valid: false, diagnostics: [{ code: 'yaml.top.unknown', path: 'version', message: '未知顶层字段 version' }] }) })
    editor.reset('pkg'); await editor.load('pkg/demo.yaml'); expect(await editor.validate()).toBe(false); expect(editor.diagnostics.value[0].path).toBe('version')
    expect(await editor.save()).toBe(false); expect(updateScript).not.toHaveBeenCalled(); expect(editor.error.value).toContain('版本缺失')
  })
  it('allows no-device edit and syntax validation without a target or model integration', async () => {
    api.listScripts.mockResolvedValue([{ id: 'pkg/demo.yaml', name: 'demo.yaml' }]); api.listExtensions.mockResolvedValue([])
    api.getScript.mockResolvedValue({ content: 'run: []\n', version: 'v1' }); api.callExtension.mockResolvedValue({ valid: true, diagnostics: [] })
    const context = { runKind: 'script', packageId: 'pkg', store: { deviceId: '', running: false }, runScript: vi.fn() }
    const wrapper = mount(SourceYamlEditor, { props: { context }, global: { stubs: { RunDetails: true, RunErrorLocation: true } } }); await flushPromises()
    try {
      expect(wrapper.get('textarea').element.value).toContain('run: []')
      const button = label => wrapper.findAll('button').find(item => item.text() === label)
      expect(button('运行脚本').attributes('disabled')).toBeDefined(); expect(button('交给 AI 分析')).toBeUndefined()
      await button('校验源码').trigger('click'); await flushPromises(); expect(wrapper.text()).toContain('源码校验通过')
      expect(api.callExtension).toHaveBeenCalledWith('gamer-yaml', 'automation.validate_source', { package_id: 'pkg', yaml: 'run: []\n', kind: 'script' })
    } finally { wrapper.unmount() }
  })
  it('offers work-draft creation from an empty library without creating source or starting a run', async () => {
    api.listScripts.mockResolvedValue([]); api.listExtensions.mockResolvedValue([])
    const context = { packageId: 'pkg', store: { deviceId: 'phone', running: false }, stopScript: vi.fn(), runScript: vi.fn() }
    const wrapper = mount(SourceYamlEditor, { props: { context } }); await flushPromises()
    try {
      expect(wrapper.text()).toContain('当前配置包还没有脚本')
      expect(wrapper.findAll('button').some(button => button.text() === '停止脚本')).toBe(false)
      await wrapper.findAll('button').find(button => button.text() === '创建工作稿').trigger('click')
      expect(wrapper.emitted('generate')).toHaveLength(1)
      expect(context.runScript).not.toHaveBeenCalled()
      expect(api.createScript).not.toHaveBeenCalled()
    } finally { wrapper.unmount() }
  })
})

describe('trace evidence and imported samples', () => {
  it('removes compacted image IDs and exposes production sampling without inventing frames', async () => {
    const run=ref('compact-run'), visible=ref(true)
    const client={getRunTrace:vi.fn().mockResolvedValueOnce({status:'available',enabled:true,images:[{image_id:'first'},{image_id:'old'}],image_ids:['first','old'],next:2}).mockResolvedValueOnce({status:'available',enabled:true,images:[{image_id:'last'}],image_ids:['first','last'],capture_policy:'production',sampled_out:30,compacted_images:10,next:3})}
    const scope=effectScope();scopes.push(scope)
    const trace=scope.run(()=>useRunTrace(run,visible,client));await flushPromises();await trace.refresh()
    expect(trace.images.value.map(image=>image.image_id)).toEqual(['first','last'])
    expect(trace.collection.value).toEqual({policy:'production',sampled:30,compacted:10})
  })
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


describe('historical template evidence', () => {
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
  const yaml = '# immutable evidence\nrun: [{finish: {template: done.png}}]'
  const wrapper = mount(RunErrorLocation, { props: { event: { path: 'run[0]', trace: { run_id: 'run1', frame_id: 7, source }, source_snapshot: { trace: { run_id: 'run1', entry: source, functions: {} }, _source_files: { 'automations/daily.yaml': yaml } } } }, global: { stubs: { Teleport: true } } })
  try { await flushPromises(); expect(wrapper.get('[aria-label="运行版本源码"]').text()).toBe(yaml); expect(wrapper.text()).toContain('执行时冻结版本'); expect(api.getScript).not.toHaveBeenCalled() } finally { wrapper.unmount() }
})


it('a late resource selection cannot change the new package selection', async () => {
  const old = pending()
  api.listExtensions.mockResolvedValue([])
  api.listScripts.mockImplementation(async pkg => pkg === 'pkg' ? [{ id: 'pkg/one.yaml', name: 'one.yaml' }, { id: 'pkg/two.yaml', name: 'two.yaml' }] : [{ id: 'other/new.yaml', name: 'new.yaml' }])
  api.getScript.mockImplementation(id => id === 'pkg/two.yaml' ? old.promise : Promise.resolve({ id, content: 'run: []', version: id }))
  const context = reactive({ packageId: 'pkg', runKind: 'script', store: { running: false, deviceId: '' }, selScript: '' })
  const wrapper = mount(SourceYamlEditor, { props: { context }, global: { stubs: { RunDetails: true, RunErrorLocation: true } } }); await flushPromises()
  try {
    await wrapper.get('[aria-label="选择源码文件"]').setValue('pkg/two.yaml'); await flushPromises()
    context.packageId = 'other'; await flushPromises()
    old.resolve({ id: 'pkg/two.yaml', content: 'old source', version: 'old' }); await flushPromises()
    expect(context.selScript).toBe('other/new.yaml'); expect(wrapper.get('[aria-label="选择源码文件"]').element.value).toBe('other/new.yaml'); expect(wrapper.get('textarea').element.value).not.toBe('old source')
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
    expect(wrapper.text()).toContain('配合教学步骤和操作记录生成脚本')
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



it('source diff retains exact changed lines', () => { expect(sourceDiff('a\nb\nc', 'a\nd\nc')).toBe('@@ 第 2 行 @@\n- b\n+ d'); expect(sourceDiff('a', 'a')).toBe('没有源码变化') })
