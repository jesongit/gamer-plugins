// @vitest-environment happy-dom
import { mount, flushPromises } from '@vue/test-utils'
import { afterEach, expect, it, vi } from 'vitest'
import Modal from './ParallelRunModal.vue'
import { api } from '../../../../../../web/src/api'
import { devicesData, runRegistry } from '../../../../../../web/src/store'
vi.mock('../../../../../../web/src/api', () => ({ api: { getEntrypointParams: vi.fn(), run: vi.fn(), getRun: vi.fn() } }))
let wrapper
afterEach(() => { wrapper?.unmount(); vi.clearAllMocks(); devicesData.value = []; runRegistry.byId = Object.create(null); runRegistry.activeByDevice = Object.create(null) })
it('edits one JSON list, blocks invalid JSON and starts selected accounts with isolated results', async () => {
  devicesData.value = [{ id: 'a', name: '188', status: 'offline' }, { id: 'b', name: '173', status: 'online' }]
  api.getEntrypointParams.mockResolvedValue({ schema: [{ name: 'codes', type: 'list', required: true }] })
  api.run.mockImplementation(async req => { if (req.device_id === 'b') throw new Error('账号占用'); return { run_id: 'r-a', state: 'running' } })
  api.getRun.mockResolvedValue({ run_id: 'r-a', device_id: 'a', state: 'success' })
  wrapper = mount(Modal, { props: { open: true, packageId: 'hsr', entrypoint: 'hsr/redeem.yaml', currentDevice: 'a' } })
  await flushPromises()
  await wrapper.findAll('input[type="checkbox"]')[1].setValue(true)
  const input = wrapper.get('[aria-label="codes"]')
  await input.setValue('["old valid code"]')
  await input.setValue('[invalid')
  await wrapper.findAll('button').find(b => b.text().startsWith('同时运行')).trigger('click')
  expect(api.run).not.toHaveBeenCalled()
  const codes = ['永远不会抛弃你', '永远不会欺骗你', '星神阿哈祝福你']
  await input.setValue(JSON.stringify(codes))
  await wrapper.findAll('button').find(b => b.text().startsWith('同时运行')).trigger('click')
  await flushPromises()
  expect(api.run.mock.calls.map(([req]) => req.device_id)).toEqual(['a', 'b'])
  expect(api.run.mock.calls.every(([req]) => JSON.stringify(req.payload.args.codes) === JSON.stringify(codes))).toBe(true)
  expect(wrapper.text()).toContain('成功'); expect(wrapper.text()).toContain('账号占用')
  await wrapper.findAll('button').at(-1).trigger('click')
  expect(wrapper.emitted('close')).toHaveLength(1)
  expect(runRegistry.byId['r-a'].state).toBe('success')
})
