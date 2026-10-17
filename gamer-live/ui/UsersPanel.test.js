import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
const mock = vi.hoisted(() => ({ call: vi.fn() }))
vi.mock('../../../web/src/api', () => ({ api: { callExtension: mock.call } }))
import UsersPanel from './UsersPanel.vue'
let wrapper
const user = { id: 'u1', open_id: 'complete-open-id-012345678901234567890', name: '观众一', balance: 3, reserved: 2, available: 1, unlimited: false, version: 1 }
const button = text => wrapper.findAll('button').find(b => b.text() === text)
beforeEach(() => {
  mock.call.mockReset().mockImplementation(async (_, action, values) => {
    if (action === 'users.read') return { users: [structuredClone(user)], has_more: false, next_offset: 1, settings: { default_quota: 1, version: 1 } }
    if (action === 'users.update') return { ...user, balance: values.balance, unlimited: values.unlimited, available: values.balance - user.reserved, version: 2 }
    if (action === 'users.settings.save') return { default_quota: values.default_quota, version: 2 }
    throw new Error('Unexpected action ' + action)
  })
})
afterEach(() => wrapper?.unmount())
async function open() { wrapper = mount(UsersPanel); await flushPromises() }
it('只读取用户，完整展示身份与余额/预占/可用，默认额度修改单独保存', async () => {
  await open()
  expect(mock.call.mock.calls.map(c => c[1])).toEqual(['users.read'])
  expect(wrapper.text()).toContain(user.open_id)
  expect(wrapper.findAll('tbody tr td').slice(0, 3).map(cell => cell.text())).toEqual(['3', '2', '1'])
  await wrapper.get('[aria-label="新人默认额度"]').setValue(5)
  await wrapper.get('form.default-form').trigger('submit'); await flushPromises()
  expect(mock.call).toHaveBeenCalledWith('gamer-live', 'users.settings.save', { default_quota: 5, expected_version: 1 })
  expect(mock.call.mock.calls.some(c => c[1] === 'users.update')).toBe(false)
  expect(wrapper.text()).toContain('已有用户的余额保持原值')
})
it('余额不能低于预占，更新携带版本和请求ID，无限余额仍保留预占', async () => {
  await open(); await button('编辑').trigger('click')
  await wrapper.get('[aria-label="用户余额"]').setValue(1)
  expect(button('保存用户额度').element.disabled).toBe(true)
  await wrapper.get('form.user-editor').trigger('submit'); await flushPromises()
  expect(mock.call.mock.calls.some(c => c[1] === 'users.update')).toBe(false)
  await wrapper.get('[aria-label="用户余额"]').setValue(4)
  await wrapper.get('.user-editor input[type="checkbox"]').setValue(true)
  await wrapper.get('form.user-editor').trigger('submit'); await flushPromises()
  expect(mock.call).toHaveBeenCalledWith('gamer-live', 'users.update', { id: 'u1', balance: 4, unlimited: true, expected_version: 1, request_id: expect.any(String) })
  expect(wrapper.findAll('tbody tr td').slice(0, 3).map(cell => cell.text())).toEqual(['∞', '2', '∞'])
})
it('搜索传完整open_id，分页追加；冲突保留编辑值且不显示成功', async () => {
  await open()
  mock.call.mockResolvedValueOnce({ users: [user], has_more: true, next_offset: 50, settings: { default_quota: 1, version: 1 } })
  await wrapper.get('[aria-label="搜索用户名或 open_id"]').setValue(user.open_id)
  await wrapper.get('form.search-form').trigger('submit'); await flushPromises()
  expect(mock.call).toHaveBeenLastCalledWith('gamer-live', 'users.read', { search: user.open_id, offset: 0 })
  mock.call.mockResolvedValueOnce({ users: [{ ...user, id: 'u2', name: '下一页' }], has_more: false, next_offset: 51 })
  await button('加载更多用户').trigger('click'); await flushPromises()
  expect(mock.call).toHaveBeenLastCalledWith('gamer-live', 'users.read', { search: user.open_id, offset: 50 })
  expect(wrapper.findAll('tbody tr')).toHaveLength(2)
  await wrapper.findAll('button').find(b => b.attributes('aria-label') === '编辑 观众一 额度').trigger('click')
  await wrapper.get('[aria-label="用户余额"]').setValue(7)
  mock.call.mockRejectedValueOnce(new Error('额度已变更'))
  await wrapper.get('form.user-editor').trigger('submit'); await flushPromises()
  expect(wrapper.get('[role="alert"]').text()).toContain('额度已变更')
  expect(wrapper.get('[aria-label="用户余额"]').element.value).toBe('7')
  expect(wrapper.text()).not.toContain('用户额度已保存')
})
