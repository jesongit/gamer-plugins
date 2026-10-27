import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
const mock = vi.hoisted(() => ({ call: vi.fn() }))
vi.mock('../../../web/src/api', () => ({ api: { callExtension: mock.call } }))
import SenderSettings from './SenderSettings.vue'
let wrapper, saved
const button = text => wrapper.findAll('button').find(b => b.text() === text)
beforeEach(() => {
  saved = { enabled: true, configured: true, account_name: '发言账号', room_id: '12345', has_session: true, has_csrf: true, version: 'v1', templates: { accepted: '已排队', success: '已完成' } }
  mock.call.mockReset().mockImplementation(async (_, action, values) => {
    if (action === 'sender.read') return structuredClone(saved)
    if (action === 'sender.save') return { ...saved, ...values, version: 'v2' }
    if (action === 'sender.clear') return { enabled: false, configured: false, has_session: false, has_csrf: false, room_id: '', templates: {}, version: 'v2' }
    if (action === 'sender.send') return { queued: true }
    throw new Error('Unexpected action ' + action)
  })
})
afterEach(() => wrapper?.unmount())
async function open() { wrapper = mount(SenderSettings); await flushPromises() }
it('挂载只读取，密文不回填；留空保存沿用且保留全部提醒模板', async () => {
  await open()
  expect(mock.call.mock.calls.map(c => c[1])).toEqual(['sender.read'])
  expect(wrapper.get('[aria-label="SESSDATA"]').element.value).toBe('')
  expect(wrapper.get('[aria-label="bili_jct"]').element.value).toBe('')
  expect(wrapper.text()).toContain('单独配置发言账号；凭据留空保留')
  expect(wrapper.get('details').attributes('open')).toBeUndefined()
  await wrapper.findAll('form')[0].trigger('submit'); await flushPromises()
  const values = mock.call.mock.calls.find(c => c[1] === 'sender.save')[2]
  expect(values).toMatchObject({ enabled: true, room_id: '12345', expected_version: 'v1', templates: { accepted: '已排队', success: '已完成', timeout: '' } })
  expect(values).not.toHaveProperty('session_cookie'); expect(values).not.toHaveProperty('csrf_token')
})
it('秘密更新成功即清空；手动发送仅明确提交队列且请求去重', async () => {
  await open()
  await wrapper.get('[aria-label="SESSDATA"]').setValue('fixture-cookie')
  await wrapper.get('[aria-label="bili_jct"]').setValue('fixture-csrf')
  await wrapper.findAll('form')[0].trigger('submit'); await flushPromises()
  expect(wrapper.get('[aria-label="SESSDATA"]').element.value).toBe('')
  expect(wrapper.get('[aria-label="bili_jct"]').element.value).toBe('')
  expect(mock.call.mock.calls.some(c => c[1] === 'sender.send')).toBe(false)
  await wrapper.get('[aria-label="手动弹幕内容"]').setValue('本地模拟文字')
  await wrapper.get('form.manual-send').trigger('submit'); await flushPromises()
  expect(mock.call).toHaveBeenCalledWith('gamer-live', 'sender.send', { text: '本地模拟文字', request_id: expect.any(String) })
  expect(wrapper.text()).toContain('弹幕已提交发送队列')
  expect(wrapper.text()).not.toContain('弹幕已发送成功')
})
it('保存失败保留草稿；清除账号后不能发送', async () => {
  await open(); await wrapper.get('[aria-label="SESSDATA"]').setValue('fixture-draft')
  mock.call.mockRejectedValueOnce(new Error('配置冲突'))
  await wrapper.findAll('form')[0].trigger('submit'); await flushPromises()
  expect(wrapper.get('[role="alert"]').text()).toContain('配置冲突')
  expect(wrapper.get('[aria-label="SESSDATA"]').element.value).toBe('fixture-draft')
  await button('清除发言账号').trigger('click'); await flushPromises()
  expect(mock.call).toHaveBeenCalledWith('gamer-live', 'sender.clear', { expected_version: 'v1' })
  expect(wrapper.get('[aria-label="手动弹幕内容"]').element.disabled).toBe(true)
  await wrapper.get('form.manual-send').trigger('submit'); await flushPromises()
  expect(mock.call.mock.calls.some(c => c[1] === 'sender.send')).toBe(false)
})
it('手动正文超过33字不提交，刷新状态不覆盖编辑草稿和版本', async () => {
  await open(); await wrapper.get('[aria-label="手动弹幕内容"]').setValue('长'.repeat(34))
  await wrapper.get('form.manual-send').trigger('submit'); await flushPromises()
  expect(mock.call.mock.calls.some(c => c[1] === 'sender.send')).toBe(false)
  await wrapper.get('[aria-label="SESSDATA"]').setValue('fixture-unsaved')
  mock.call.mockResolvedValueOnce({ ...saved, version: 'remote-v2', last_error: '账号已过期' })
  await button('刷新发言状态').trigger('click'); await flushPromises()
  expect(wrapper.text()).toContain('账号已过期')
  expect(wrapper.get('[aria-label="SESSDATA"]').element.value).toBe('fixture-unsaved')
  await wrapper.findAll('form')[0].trigger('submit'); await flushPromises()
  expect(mock.call.mock.calls.find(c => c[1] === 'sender.save')[2].expected_version).toBe('v1')
})
it('提醒模板超长或含换行不保存', async () => {
  await open(); await wrapper.get('[aria-label="加入队列提醒文字"]').setValue('字'.repeat(201))
  await wrapper.findAll('form')[0].trigger('submit'); await flushPromises()
  expect(wrapper.get('[role="alert"]').text()).toContain('最多 200')
  expect(mock.call.mock.calls.some(c => c[1] === 'sender.save')).toBe(false)
})
