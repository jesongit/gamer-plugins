// @vitest-environment happy-dom
import { mount, flushPromises } from '@vue/test-utils'
import { defineComponent, ref } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import Panel from './AutomationGenerationPanel.vue'
import SourceYamlEditor from './SourceYamlEditor.vue'
import { useConsoleTemplates } from './useConsoleTemplates'
import { notifyYamlResourcesChanged, yamlResourcesChanged } from './resource-changes'
import { api } from '../../../../../../web/src/api'
import { WORKSPACE_CONTEXT_KEY } from '../../../../../../web/src/workspace/context'
import { pluginMessageChannel } from '../../../../../../web/src/workspace/plugin-messages'
const { confirm } = vi.hoisted(() => ({ confirm: Object.assign(vi.fn(), { cancel: vi.fn() }) }))
vi.mock('../../../../../../web/src/components/ui/useConfirmDialog', () => ({ useConfirmDialog: () => confirm }))
vi.mock('../../../../../../web/src/api', () => ({ api: {
  callExtension: vi.fn(), listTemplates: vi.fn(), listScripts: vi.fn(), listExtensions: vi.fn(), getScript: vi.fn(),
  getRun: vi.fn(), getRunTrace: vi.fn(), cancelRun: vi.fn(), deleteScript: vi.fn(), deleteTemplate: vi.fn(),
} }))
const wrappers = []
const button = (wrapper, text) => wrapper.findAll('button').find(b => b.text() === text)
const pending = () => { let resolve; const promise = new Promise(done => { resolve = done }); return { promise, resolve } }
const makeDoc = (id = 'daily.yaml') => ({ script_id: id, head_version: '0.1', published_version: null, job: null,
  versions: [{ version: '0.1', yaml: '# saved work\nrun: []\n', diagnostics: [], published: false }] })
let doc, deleted
beforeEach(() => {
  vi.resetAllMocks(); confirm.mockResolvedValue(true); doc = makeDoc(); deleted = false
  yamlResourcesChanged.packageId = ''; yamlResourcesChanged.seq = 0
  Object.assign(pluginMessageChannel('gamer-yaml:open-generation'), { packageId: '', scriptId: '', importScriptId: '', repairRun: null, seq: 0 })
  api.listTemplates.mockResolvedValue([{ name: 'old.png', pkg: 'pkg' }])
  api.listScripts.mockResolvedValue([{ id: 'pkg/daily.yaml', pkg: 'pkg', name: 'daily.yaml' }])
  api.listExtensions.mockResolvedValue([])
  api.getScript.mockResolvedValue({ id: 'pkg/daily.yaml', content: '# formal source\nrun: []\n', version: 'resource-1' })
  api.callExtension.mockImplementation(async (_plugin, action) => {
    if (action === 'authoring.list') return { scripts: deleted ? [] : [{ script_id: doc.script_id, head_version: doc.head_version }] }
    if (action === 'authoring.get' || action === 'authoring.import') return { script: doc }
    if (action === 'generation.readiness') return { ready: true }
    if (action === 'sample.list') return { samples: [] }
    if (action === 'authoring.delete_preview') return { script_id: doc.script_id, head_version: doc.head_version,
      version_count: 2, generated_template_count: 3, private_run_count: 4, busy: false }
    if (action === 'authoring.delete') { deleted = true; return { deleted: true, script_id: doc.script_id } }
    if (action === 'authoring.publish') return { script: { ...doc, published_version: '1.0', head_version: '1.0',
      versions: [...doc.versions, { ...doc.versions[0], version: '1.0', published: true }] } }
    if (action === 'authoring.run') return { run_id: 'private-test' }
    return {}
  })
})
afterEach(() => { wrappers.splice(0).forEach(w => w.unmount()); localStorage.clear(); vi.useRealTimers() })
async function panel(options = {}) {
  const wrapper = mount(Panel, { props: { packageId: 'pkg' }, ...options }); wrappers.push(wrapper); await flushPromises(); return wrapper
}
function templateConsumer(packageId = ref('pkg')) {
  const templatesData = ref([])
  const Consumer = defineComponent({ setup() { useConsoleTemplates({ packageId, templatesData, store: {}, connected: ref(false), current: ref(null),
    videoElement: ref(null), videoWrap: ref(null), toast: vi.fn() }); return () => null } })
  const wrapper = mount(Consumer); wrappers.push(wrapper)
  return { templatesData, packageId }
}
describe('work draft lifecycle and formal resource refresh', () => {
  it('finishes publication and deletion while shared material loading is pending', async () => {
    const delayed = pending(), implementation = api.callExtension.getMockImplementation()
    api.callExtension.mockImplementation((plugin, action, values) =>
      action === 'sample.list' ? delayed.promise : implementation(plugin, action, values))
    const wrapper = await panel()
    await button(wrapper, '发布到脚本库').trigger('click'); await flushPromises()
    expect(button(wrapper, '删除工作稿').attributes('disabled')).toBeUndefined()
    expect(await wrapper.vm.beforeTabChange()).toBe(true)
    doc = { ...doc, head_version: '1.0' }
    await button(wrapper, '删除工作稿').trigger('click'); await flushPromises()
    expect(wrapper.find('[aria-label="脚本 YAML"]').exists()).toBe(false)
    expect(button(wrapper, '从脚本库导入').attributes('disabled')).toBeUndefined()
    delayed.resolve({ samples: [] }); await flushPromises()
  })
  it('publishing refreshes an already mounted same-package template list and material inventory', async () => {
    const consumer = templateConsumer(), wrapper = await panel(); await flushPromises()
    api.listTemplates.mockResolvedValue([{ name: 'published.png', pkg: 'pkg' }])
    const before = yamlResourcesChanged.seq
    await button(wrapper, '发布到脚本库').trigger('click'); await flushPromises()
    expect(yamlResourcesChanged.seq).toBe(before + 1)
    expect(yamlResourcesChanged).toMatchObject({ packageId: 'pkg', scriptId: 'daily.yaml' })
    expect(consumer.templatesData.value.map(t => t.name)).toEqual(['published.png'])
    expect(wrapper.text()).toContain('published.png')
    expect(api.callExtension.mock.calls.filter(c => c[1] === 'authoring.generate')).toHaveLength(0)
  })
  it('ignores other packages and rejects late template responses after a package switch', async () => {
    const delayed = pending(); api.listTemplates.mockImplementation(pkg => pkg === 'pkg' ? delayed.promise : Promise.resolve([{ name: 'other.png', pkg }]))
    const consumer = templateConsumer(); consumer.packageId.value = 'other'; await flushPromises()
    delayed.resolve([{ name: 'obsolete.png', pkg: 'pkg' }]); await flushPromises()
    expect(consumer.templatesData.value.map(t => t.name)).toEqual(['other.png'])
    const count = api.listTemplates.mock.calls.length
    notifyYamlResourcesChanged('pkg', 'daily.yaml'); await flushPromises()
    expect(api.listTemplates.mock.calls).toHaveLength(count)
  })
  it('deletes only the private draft in one confirmation, including unsaved edits and ended private runs', async () => {
    const wrapper = await panel()
    await wrapper.get('[aria-label="脚本 YAML"]').setValue('# unsaved\nrun: []\n')
    await button(wrapper, '删除工作稿').trigger('click'); await flushPromises()
    expect(confirm).toHaveBeenCalledTimes(1)
    expect(confirm.mock.calls[0][0]).toContain('2 个私有版本、3 个私有生成模板和 4 次')
    expect(confirm.mock.calls[0][0]).toContain('当前未保存修改也会丢弃')
    expect(confirm.mock.calls[0][0]).toContain('正式文件、正式模板、发布记录和共享原素材均保留')
    expect(api.callExtension).toHaveBeenCalledWith('gamer-yaml', 'authoring.delete', { package_id: 'pkg', script_id: 'daily.yaml', expected_version: '0.1', remove_private_runs: true })
    expect(api.deleteScript).not.toHaveBeenCalled(); expect(api.deleteTemplate).not.toHaveBeenCalled()
    expect(wrapper.find('[aria-label="脚本 YAML"]').exists()).toBe(false)
    expect(wrapper.get('[aria-label="指定脚本"]').element.value).toBe('')
    expect(JSON.parse(localStorage.getItem('gamer-yaml.authoring.pkg')).scriptId).toBe('')
    expect(api.callExtension.mock.calls.filter(c => c[1] === 'authoring.import')).toHaveLength(0)
  })
  it('cancelling deletion retains dirty source without saving or deleting anything', async () => {
    confirm.mockResolvedValue(false); const wrapper = await panel()
    await wrapper.get('[aria-label="脚本 YAML"]').setValue('# keep unsaved\nrun: []\n')
    await button(wrapper, '删除工作稿').trigger('click'); await flushPromises()
    expect(wrapper.get('[aria-label="脚本 YAML"]').element.value).toContain('# keep unsaved')
    expect(api.callExtension.mock.calls.some(c => ['authoring.save', 'authoring.delete'].includes(c[1]))).toBe(false)
  })
  it('an older list refresh cannot restore a deleted draft', async () => {
    const wrapper = await panel(), delayed = pending(), implementation = api.callExtension.getMockImplementation()
    let oldRefresh = true
    api.callExtension.mockImplementation((plugin, action, values) => {
      if (action === 'authoring.list' && oldRefresh) { oldRefresh = false; return delayed.promise }
      return implementation(plugin, action, values)
    })
    await button(wrapper, '刷新').trigger('click')
    await button(wrapper, '删除工作稿').trigger('click'); await flushPromises()
    delayed.resolve({ scripts: [{ script_id: 'daily.yaml', head_version: '0.1' }] }); await flushPromises()
    expect(wrapper.get('[aria-label="指定脚本"]').element.value).toBe('')
    expect(wrapper.find('[aria-label="脚本 YAML"]').exists()).toBe(false)
  })
  it('does not delete when backend preview finds an active private test', async () => {
    const wrapper = await panel(), implementation = api.callExtension.getMockImplementation()
    api.callExtension.mockImplementation((plugin, action, values) => action === 'authoring.delete_preview' ? Promise.resolve({ head_version: '0.1', busy: true }) : implementation(plugin, action, values))
    await button(wrapper, '删除工作稿').trigger('click'); await flushPromises()
    expect(wrapper.get('[role="alert"]').text()).toContain('正在编辑或测试')
    expect(confirm).not.toHaveBeenCalled(); expect(api.callExtension.mock.calls.some(c => c[1] === 'authoring.delete')).toBe(false)
  })
  it('disables deletion during generation and the current private run', async () => {
    doc.job = { state: 'generating' }; const generating = await panel()
    expect(button(generating, '删除工作稿').attributes('disabled')).toBeDefined(); generating.unmount(); wrappers.pop()
    doc.job = null
    const running = await panel({ global: { provide: { [WORKSPACE_CONTEXT_KEY]: { getSnapshot: () => ({ deviceId: 'device' }) } } } })
    await button(running, '测试当前版本').trigger('click'); await flushPromises()
    expect(button(running, '删除工作稿').attributes('disabled')).toBeDefined()
  })
  it('discarded package preview cannot show a confirmation or delete another draft', async () => {
    const wrapper = await panel(), delayed = pending(), implementation = api.callExtension.getMockImplementation()
    api.callExtension.mockImplementation((plugin, action, values) => action === 'authoring.delete_preview' ? delayed.promise : implementation(plugin, action, values))
    await button(wrapper, '删除工作稿').trigger('click'); await wrapper.setProps({ packageId: 'other' }); await flushPromises()
    delayed.resolve({ head_version: '0.1', busy: false }); await flushPromises()
    expect(confirm).not.toHaveBeenCalled(); expect(api.callExtension.mock.calls.some(c => c[1] === 'authoring.delete')).toBe(false)
  })
  it('exposes formal scripts only through explicit import, without reopening a deleted draft', async () => {
    deleted = true; const wrapper = await panel()
    expect(wrapper.find('[aria-label="脚本 YAML"]').exists()).toBe(false)
    await button(wrapper, '从脚本库导入').trigger('click')
    await wrapper.get('[aria-label="导入脚本库文件"]').setValue('daily.yaml')
    await wrapper.get('.import-form').trigger('submit'); await flushPromises()
    expect(api.callExtension).toHaveBeenCalledWith('gamer-yaml', 'authoring.import', { package_id: 'pkg', script_id: 'daily.yaml' })
    expect(wrapper.get('[aria-label="脚本 YAML"]').element.value).toBe(doc.versions[0].yaml)
    expect(api.callExtension.mock.calls.some(c => ['authoring.generate', 'authoring.publish'].includes(c[1]))).toBe(false)
  })
  it('a script-library version-management request preselects import without creating a draft', async () => {
    deleted = true
    Object.assign(pluginMessageChannel('gamer-yaml:open-generation'), { packageId: 'pkg', scriptId: 'daily.yaml', importScriptId: 'daily.yaml', seq: 1 })
    const wrapper = await panel()
    expect(wrapper.get('[aria-label="导入脚本库文件"]').element.value).toBe('daily.yaml')
    expect(wrapper.text()).toContain('请从脚本库导入后继续修改')
    expect(api.callExtension.mock.calls.some(c => ['authoring.import', 'authoring.create'].includes(c[1]))).toBe(false)
  })
  it('preserves unsaved script-library source while refreshing its resource list', async () => {
    const context = { packageId: 'pkg', store: { deviceId: 'device', runId: '' }, selScript: 'pkg/daily.yaml', sourcePackage: 'pkg' }
    const wrapper = mount(SourceYamlEditor, { props: { context } }); wrappers.push(wrapper); await flushPromises()
    const source = wrapper.get('[aria-label="YAML 原文"]')
    await source.setValue('# personal unsaved changes\nrun: []\n')
    api.listScripts.mockResolvedValue([{ id: 'pkg/daily.yaml', name: 'new formal label' }])
    const reads = api.getScript.mock.calls.length
    notifyYamlResourcesChanged('pkg', 'daily.yaml'); await flushPromises()
    expect(source.element.value).toContain('# personal unsaved changes')
    expect(api.getScript.mock.calls).toHaveLength(reads)
    expect(wrapper.text()).toContain('new formal label')
  })
  it('saving edited formal source explicitly imports a missing draft then saves a private version', async () => {
    const wrapper = mount(SourceYamlEditor, { props: { context: { packageId: 'pkg', store: { deviceId: 'device', runId: '' }, selScript: 'pkg/daily.yaml' } } })
    wrappers.push(wrapper); await flushPromises()
    await wrapper.get('[aria-label="YAML 原文"]').setValue('# my edit\nrun: []\n')
    const implementation = api.callExtension.getMockImplementation()
    api.callExtension.mockImplementation((plugin, action, values) => {
      if (action === 'authoring.get') return Promise.reject(new Error('authoring.draft_not_found'))
      if (action === 'authoring.save') return Promise.resolve({ script: { ...doc, head_version: '1.1', versions: [{ version: '1.1', yaml: values.yaml, diagnostics: [] }] } })
      return implementation(plugin, action, values)
    })
    await button(wrapper, '保存草稿').trigger('click'); await flushPromises()
    expect(api.callExtension).toHaveBeenCalledWith('gamer-yaml', 'authoring.import', { package_id: 'pkg', script_id: 'daily.yaml' })
    expect(api.callExtension).toHaveBeenCalledWith('gamer-yaml', 'authoring.save', expect.objectContaining({ yaml: '# my edit\nrun: []\n', expected_version: '0.1' }))
    expect(wrapper.text()).toContain('已保存小版本 1.1')
    expect(api.callExtension.mock.calls.some(c => c[1] === 'authoring.publish')).toBe(false)
  })
  it('a failed draft lookup is not treated as a missing draft for unrelated errors', async () => {
    const wrapper = mount(SourceYamlEditor, { props: { context: { packageId: 'pkg', store: { deviceId: 'device', runId: '' }, selScript: 'pkg/daily.yaml' } } })
    wrappers.push(wrapper); await flushPromises()
    await wrapper.get('[aria-label="YAML 原文"]').setValue('# keep edit\nrun: []\n')
    api.callExtension.mockRejectedValue(new Error('network failed'))
    await button(wrapper, '保存草稿').trigger('click'); await flushPromises()
    expect(wrapper.text()).toContain('network failed')
    expect(api.callExtension.mock.calls.some(c => ['authoring.import', 'authoring.create', 'authoring.save'].includes(c[1]))).toBe(false)
  })
  it('saved private source requests its exact version parameters before submitting a string list', async () => {
    const context = { packageId: 'pkg', store: { deviceId: 'device', runId: '' }, selScript: 'pkg/daily.yaml' }
    const wrapper = mount(SourceYamlEditor, { props: { context } }); wrappers.push(wrapper); await flushPromises()
    const yaml = 'params:\n  codes: {type: list, required: true}\nrun: []\n'
    await wrapper.get('[aria-label="YAML 原文"]').setValue(yaml)
    const implementation = api.callExtension.getMockImplementation()
    api.callExtension.mockImplementation((plugin, action, values) => {
      if (action === 'authoring.save') { doc = { ...doc, head_version: '0.2', versions: [{ version: '0.2', yaml, parameter_schema: [{ name: 'codes', type: 'list', required: true }], diagnostics: [] }] }; return Promise.resolve({ script: doc }) }
      return implementation(plugin, action, values)
    })
    await button(wrapper, '保存并运行').trigger('click'); await flushPromises()
    expect(api.callExtension.mock.calls.some(c => c[1] === 'authoring.run')).toBe(false)
    const codes = ['甲', '乙', '丙']
    await wrapper.get('[aria-label="codes"]').setValue(JSON.stringify(codes))
    await button(wrapper, '▶ 运行').trigger('click'); await flushPromises()
    expect(api.callExtension).toHaveBeenCalledWith('gamer-yaml', 'authoring.run', { package_id: 'pkg', script_id: 'daily.yaml', version: '0.2', expected_version: '0.2', device_id: 'device', args: { codes } })
  })
})
