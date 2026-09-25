import { beforeEach, expect, it, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
const mocks = vi.hoisted(() => ({ call: vi.fn(), confirm: vi.fn() }))
vi.mock('../../../web/src/api', () => ({ api: { listPackages: async () => ({ packages: [{ id: 'demo', version: '1.0.0' }] }), callExtension: mocks.call } }))
vi.mock('../../../web/src/components/ui/useConfirmDialog', () => ({ useConfirmDialog: () => mocks.confirm }))
import Publisher from './Publisher.vue'
const job = { id: 'job', repository: 'owner/repo', account: 'owner', tag: 'catalog-1', state: 'prepared', catalog: { packages: [{ id: 'demo', name: '示例', version: '1.0.0', size: 123, sha256: 'a'.repeat(64) }] } }
beforeEach(() => {
  vi.resetAllMocks()
  mocks.call.mockImplementation(async (_, action) => action === 'publisher.jobs' ? [] : { ...job, state: action === 'publisher.draft' ? 'draft' : action === 'publisher.publish' ? 'published' : 'prepared' })
})
async function prepare(w) {
  await flushPromises(); await w.get('input[placeholder]').setValue('owner/repo'); await w.get('input[value="demo"]').setValue(true)
  await w.findAll('button').find(b => b.text() === '生成发布预览').trigger('click'); await flushPromises()
  expect(mocks.call.mock.calls.map(c => c[1]), w.html()).toContain('publisher.prepare')
  expect(w.find('.preview').exists(), w.html()).toBe(true)
}
it('预览不创建远端草稿；公开需要单独点击及确认', async () => {
  const w = mount(Publisher); await prepare(w)
  expect(mocks.call.mock.calls.map(c => c[1])).not.toContain('publisher.draft')
  await w.findAll('button').find(b => b.text().includes('创建 / 重试草稿')).trigger('click'); await flushPromises()
  mocks.confirm.mockResolvedValue(false)
  await w.findAll('button').find(b => b.text() === '公开发布').trigger('click'); await flushPromises()
  expect(mocks.call.mock.calls.map(c => c[1])).not.toContain('publisher.publish')
  mocks.confirm.mockResolvedValue(true)
  await w.findAll('button').find(b => b.text() === '公开发布').trigger('click'); await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-package-publisher', 'publisher.publish', { job_id: 'job' }); expect(w.text()).toContain('配置目录已公开'); w.unmount()
})
it('失败保留预览和重试入口，不误报成功', async () => {
  const w = mount(Publisher); await prepare(w)
  mocks.call.mockImplementation(async (_, action) => { if (action === 'publisher.draft') throw new Error('上传失败'); return [] })
  await w.findAll('button').find(b => b.text().includes('创建 / 重试草稿')).trigger('click'); await flushPromises()
  expect(w.get('[role="alert"]').text()).toContain('上传失败'); expect(w.text()).toContain('创建 / 重试草稿'); expect(w.text()).not.toContain('配置目录已公开'); w.unmount()
})
