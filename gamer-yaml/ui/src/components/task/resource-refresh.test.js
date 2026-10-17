// @vitest-environment happy-dom
import { defineComponent, h, nextTick, ref } from 'vue'
import { mount, flushPromises } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import PayloadEditor from './GamerYamlPayloadEditor.vue'
import { ensureGamerYamlResources } from './gamer-yaml-resources'
import { notifyYamlResourcesChanged, yamlResourcesChanged } from '../console/resource-changes'
import { api } from '../../../../../../web/src/api'
import { scriptsData, templatesData } from '../../../../../../web/src/store'
vi.mock('../../../../../../web/src/api', () => ({ api: { listScripts: vi.fn(), listTemplates: vi.fn(), getEntrypointParams: vi.fn() } }))
const wrappers = []
const pending = () => { let resolve; const promise = new Promise(done => { resolve = done }); return { promise, resolve } }
beforeEach(async () => {
  await ensureGamerYamlResources(null); vi.resetAllMocks(); yamlResourcesChanged.packageId = ''; yamlResourcesChanged.seq = 0
  api.listScripts.mockResolvedValue([{ id: 'pkg/daily.yaml', pkg: 'pkg', name: 'daily.yaml' }])
  api.listTemplates.mockResolvedValue([{ name: 'old.png', pkg: 'pkg' }])
  api.getEntrypointParams.mockResolvedValue({ schema: [{ name: 'count', type: 'integer', default: 2, required: false }] })
})
afterEach(() => wrappers.splice(0).forEach(w => w.unmount()))
describe('published YAML task resources', () => {
  it('invalidates same-package nonempty resource caches while leaving other packages alone', async () => {
    await ensureGamerYamlResources('pkg')
    api.listScripts.mockResolvedValue([{ id: 'pkg/new.yaml', pkg: 'pkg' }])
    api.listTemplates.mockResolvedValue([{ name: 'new.png', pkg: 'pkg' }])
    notifyYamlResourcesChanged('pkg', 'new.yaml'); await flushPromises()
    expect(scriptsData.value.map(s => s.id)).toEqual(['pkg/new.yaml'])
    expect(templatesData.value.map(t => t.name)).toEqual(['new.png'])
    const reads = api.listScripts.mock.calls.length
    notifyYamlResourcesChanged('other', 'new.yaml'); await flushPromises()
    expect(api.listScripts.mock.calls).toHaveLength(reads)
  })
  it('rejects responses from before a publish and from a previously active package', async () => {
    const obsolete = pending(); api.listTemplates.mockReturnValueOnce(obsolete.promise)
    const loading = ensureGamerYamlResources('pkg')
    api.listTemplates.mockResolvedValue([{ name: 'latest.png', pkg: 'pkg' }])
    notifyYamlResourcesChanged('pkg', 'daily.yaml'); await flushPromises()
    obsolete.resolve([{ name: 'obsolete.png', pkg: 'pkg' }]); await loading
    expect(templatesData.value.map(t => t.name)).toEqual(['latest.png'])
    const late = pending(); api.listScripts.mockReturnValueOnce(late.promise)
    const previous = ensureGamerYamlResources('pkg', true)
    api.listScripts.mockResolvedValue([{ id: 'other/other.yaml', pkg: 'other' }])
    api.listTemplates.mockResolvedValue([{ name: 'other.png', pkg: 'other' }])
    await ensureGamerYamlResources('other')
    late.resolve([{ id: 'pkg/late.yaml', pkg: 'pkg' }]); await previous
    expect(scriptsData.value.map(s => s.id)).toEqual(['other/other.yaml'])
  })
  it('reloads the published entrypoint schema without clearing current user args', async () => {
    const args = ref({ count: 7 }), entrypoint = ref('pkg/daily.yaml')
    const Host = defineComponent({ setup() { return () => h(PayloadEditor, { entrypoint: entrypoint.value,
      ctx: { packageId: 'pkg', deviceId: '' }, payload: { args: args.value }, 'onUpdate:payload': value => { args.value = value.args } }) } })
    const wrapper = mount(Host); wrappers.push(wrapper); await flushPromises()
    expect(wrapper.get('[aria-label="count"]').element.value).toBe('7')
    await wrapper.get('[aria-label="count"]').setValue('11'); await nextTick()
    const before = api.getEntrypointParams.mock.calls.length
    api.getEntrypointParams.mockResolvedValue({ schema: [{ name: 'count', type: 'integer', default: 5, required: false }, { name: 'bonus', type: 'integer', default: 1, required: false }] })
    notifyYamlResourcesChanged('pkg', 'daily.yaml'); await flushPromises()
    expect(api.getEntrypointParams.mock.calls).toHaveLength(before + 1)
    expect(wrapper.get('[aria-label="count"]').element.value).toBe('11')
    expect(args.value.count).toBe(11)
    expect(wrapper.text()).toContain('bonus')
    notifyYamlResourcesChanged('other', 'daily.yaml'); await flushPromises()
    expect(api.getEntrypointParams.mock.calls).toHaveLength(before + 1)
  })
})
