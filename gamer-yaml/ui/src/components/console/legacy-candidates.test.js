// @vitest-environment happy-dom
import { mount, flushPromises } from '@vue/test-utils'
import { afterEach, expect, it, vi } from 'vitest'
import Panel from './AutomationCandidateReview.vue'
import { api } from '../../../../../../web/src/api'
import { pluginMessageChannel } from '../../../../../../web/src/workspace/plugin-messages'

vi.mock('../../../../../../web/src/api', () => ({ api: { callExtension: vi.fn() } }))
let wrapper
const request = pluginMessageChannel('gamer-yaml:open-generation')
afterEach(() => {
  wrapper?.unmount(); wrapper = null; vi.resetAllMocks()
  Object.assign(request, { packageId:'', candidateId:null, scriptId:'', seq:0 })
})
const candidate = id => ({ id, name:id, revision:7, yaml:`# frozen ${id}\nrun: []\n`, templates:[], usage:{historical_tokens:123} })
function mountPanel() {
  wrapper = mount(Panel, { props:{packageId:'pkg-a'}, global:{stubs:{CandidateTemplatePreview:true}} })
}

it('reads frozen legacy records and creates a new work draft only after an explicit import', async () => {
  api.callExtension.mockImplementation(async (_id, action) => {
    if (action === 'generation.list') return { candidates:[candidate('old')] }
    if (action === 'generation.get') return { candidate:candidate('old') }
    if (action === 'generation.import_draft') return { script:{script_id:'reviewed.yaml'} }
    throw new Error(`Unexpected action ${action}`)
  })
  mountPanel(); await flushPromises()
  await wrapper.get('select').setValue('old'); await flushPromises()
  expect(wrapper.get('.source').text()).toContain('# frozen old')
  expect(wrapper.text()).toContain('123')
  expect(api.callExtension.mock.calls.every(call => ['generation.list','generation.get'].includes(call[1]))).toBe(true)
  await wrapper.get('input').setValue('reviewed.yaml')
  await wrapper.get('form').trigger('submit'); await flushPromises()
  expect(api.callExtension).toHaveBeenCalledWith('gamer-yaml','generation.import_draft',{
    package_id:'pkg-a',candidate_id:'old',new_script_id:'reviewed.yaml',
  })
  expect(request.scriptId).toBe('reviewed.yaml')
  expect(request.candidateId).toBeNull()
  expect(api.callExtension.mock.calls.some(call => /publish|repair|generate|retry|delete/.test(call[1]))).toBe(false)
})

it('discards a late old-package record and retains the new-package record', async () => {
  let completeOld
  api.callExtension.mockImplementation(async (_id, action, values) => {
    if (action === 'generation.list') return { candidates:[candidate(values.package_id)] }
    if (action === 'generation.get' && values.package_id === 'pkg-a') return new Promise(resolve => {completeOld = resolve})
    if (action === 'generation.get') return { candidate:candidate('pkg-b') }
    throw new Error(`Unexpected action ${action}`)
  })
  mountPanel(); await flushPromises()
  await wrapper.get('select').setValue('pkg-a'); await flushPromises()
  await wrapper.setProps({packageId:'pkg-b'}); await flushPromises()
  await wrapper.get('select').setValue('pkg-b'); await flushPromises()
  completeOld({candidate:candidate('pkg-a')}); await flushPromises()
  expect(wrapper.get('.source').text()).toContain('# frozen pkg-b')
  expect(wrapper.text()).not.toContain('# frozen pkg-a')
})
