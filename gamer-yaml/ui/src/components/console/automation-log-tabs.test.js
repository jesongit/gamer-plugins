// @vitest-environment happy-dom
import { mount, flushPromises } from '@vue/test-utils'
import { defineComponent, ref } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import Workbench from './AutomationWorkbench.vue'
import Panel from './AutomationGenerationPanel.vue'
import { api } from '../../../../../../web/src/api'
import { WORKSPACE_CONTEXT_KEY } from '../../../../../../web/src/workspace/context'

vi.mock('../../../../../../web/src/api', () => ({ api: { callExtension: vi.fn(), listTemplates: vi.fn() } }))
const wrappers = []
afterEach(() => { wrappers.forEach(w => w.unmount()); wrappers.length = 0; vi.resetAllMocks(); localStorage.clear() })
const context = tab => ({ scripts: { automationTab: tab, packageId: 'pkg', store: { deviceId: 'device-1', runId: '' }, selScript: '' }, templates: {} })
const Logs = defineComponent({ props: ['deviceId', 'target', 'liveRun', 'visible'], template: '<section class="log-pane">运行记录</section>' })
const clickTab = (wrapper, label) => wrapper.findAll('.tab-btn').find(b => b.text() === label).trigger('click')

describe('separate automation run logs', () => {
  it('restores the saved automation tab when the workbench is recreated', async () => {
    localStorage.setItem('gamer-yaml.authoring.pkg', JSON.stringify({ tab: 'generation' }))
    const wrapper = mount(Workbench, { props: { context: context('scripts') }, global: { stubs: { AutomationGenerationPanel: true, SourceYamlEditor: true } } }); wrappers.push(wrapper)
    await flushPromises()
    expect(wrapper.get('.tab-btn.active').text()).toBe('工作稿与版本')
  })
  it('opens existing unpublished draft revisions on the first reload without publishing them', async () => {
    api.callExtension.mockResolvedValue({ scripts: [{ script_id: 'daily.yaml', head_version: '0.22', published_version: null }] })
    const wrapper = mount(Workbench, { props: { context: context('scripts') }, global: { stubs: { AutomationGenerationPanel: true, SourceYamlEditor: true } } }); wrappers.push(wrapper)
    await flushPromises()
    expect(wrapper.get('.tab-btn.active').text()).toBe('工作稿与版本')
    expect(api.callExtension.mock.calls.every(c => c[1] === 'authoring.list')).toBe(true)
  })
  it('reads logs without discarding an unsaved editor or weakening its other navigation guard', async () => {
    const guard = vi.fn(async () => false)
    const Editor = defineComponent({ setup(_, { expose }) { const draft = ref('unsaved source'); expose({ beforeTabChange: guard }); return { draft } }, template: '<textarea v-model="draft" />' })
    const wrapper = mount(Workbench, { props: { context: context('scripts') }, global: { stubs: { SourceYamlEditor: Editor, RunDetails: Logs, TemplateCapture: true } } }); wrappers.push(wrapper)
    const editorId = wrapper.getComponent(Editor).vm.$.uid
    await wrapper.get('textarea').setValue('retained unsaved source')
    await clickTab(wrapper, '运行日志'); await flushPromises()
    expect(wrapper.getComponent(Logs).props('visible')).toBe(true)
    expect(guard).not.toHaveBeenCalled()
    await clickTab(wrapper, '脚本库'); await flushPromises()
    expect(wrapper.getComponent(Editor).vm.$.uid).toBe(editorId)
    expect(wrapper.get('textarea').element.value).toBe('retained unsaved source')
    expect(wrapper.getComponent(Logs).props('visible')).toBe(false)
    await clickTab(wrapper, '模板'); await flushPromises()
    expect(guard).toHaveBeenCalledOnce()
    expect(wrapper.get('.tab-btn.active').text()).toBe('脚本库')
  })
  it('hides every root of the template panel while showing logs and retains its state on return', async () => {
    const Templates = defineComponent({ inheritAttrs:false, setup(){return{value:ref('saved search')}}, template:'<section class="template-pane"><input v-model="value" /></section><aside class="template-secondary">second root</aside>' })
    const wrapper = mount(Workbench, { props:{context:context('templates')}, attachTo:document.body, global:{stubs:{TemplateCapture:Templates,RunDetails:Logs}} }); wrappers.push(wrapper)
    const id = wrapper.getComponent(Templates).vm.$.uid
    await wrapper.get('.template-pane input').setValue('retained search')
    await clickTab(wrapper,'运行日志'); await flushPromises()
    expect(wrapper.get('.workbench-templates').attributes('style'), wrapper.html()).toContain('display: none')
    expect(wrapper.get('.template-pane').isVisible()).toBe(false)
    expect(wrapper.get('.template-secondary').isVisible()).toBe(false)
    expect(wrapper.get('.log-pane').isVisible()).toBe(true)
    await clickTab(wrapper,'模板'); await flushPromises()
    expect(wrapper.getComponent(Templates).vm.$.uid).toBe(id)
    expect(wrapper.get('.template-pane input').element.value).toBe('retained search')
  })

  it('opens the exact private trial journal and retains draft version and instruction on return', async () => {
    const doc = { script_id: 'daily.yaml', head_version: '0.2', job: null, versions: ['0.1', '0.2'].map(version => ({ version, yaml: '# AI fixture\nrun: []\n', diagnostics: [], published: false })) }
    api.listTemplates.mockResolvedValue([])
    api.callExtension.mockImplementation(async (_id, action) => {
      if (action === 'authoring.list') return { scripts: [{ script_id: 'daily.yaml' }] }
      if (action === 'authoring.get') return { script: doc }
      if (action === 'generation.readiness') return { ready: true }
      if (action === 'sample.list') return { samples: [] }
      if (action === 'authoring.run') return { run_id: 'trial-7', version: '0.1' }
      return {}
    })
    const wrapper = mount(Workbench, { props: { context: context('generation') }, global: { stubs: { RunDetails: Logs }, provide: { [WORKSPACE_CONTEXT_KEY]: { getSnapshot: () => ({ deviceId: 'device-1' }) } } } }); wrappers.push(wrapper)
    await flushPromises()
    await wrapper.get('[aria-label="指定脚本"]').setValue('daily.yaml'); await flushPromises()
    await wrapper.get('[aria-label="脚本版本"]').setValue('0.1'); await flushPromises()
    await wrapper.get('[aria-label="版本修改说明"]').setValue('retain this instruction')
    const panelId = wrapper.getComponent(Panel).vm.$.uid
    await wrapper.findAll('button').find(b => b.text() === '测试当前版本').trigger('click'); await flushPromises()
    expect(wrapper.get('.tab-btn.active').text()).toBe('运行日志')
    expect(wrapper.getComponent(Logs).props()).toMatchObject({ deviceId: 'device-1', target: 'pkg/daily.yaml', liveRun: 'trial-7', visible: true })
    expect(wrapper.getComponent(Panel).vm.$.uid).toBe(panelId)
    expect(wrapper.find('.authoring .log-pane').exists()).toBe(false)
    await clickTab(wrapper, '工作稿与版本'); await flushPromises()
    expect(wrapper.get('[aria-label="脚本版本"]').element.value).toBe('0.1')
    expect(wrapper.get('[aria-label="版本修改说明"]').element.value).toBe('retain this instruction')
    expect(api.callExtension.mock.calls.some(c => ['authoring.save', 'authoring.generate', 'authoring.publish'].includes(c[1]))).toBe(false)
  })
})
