import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
const mock = vi.hoisted(() => ({ call: vi.fn(), saved: null }))
vi.mock('../../../web/src/api', () => ({ api: { callExtension: mock.call } }))
import AudienceSettings from './AudienceSettings.vue'

let wrapper
beforeEach(() => {
  mock.saved = { enabled: false, url: 'http://127.0.0.1:8443/local/extensions/gamer-live/read-capability/audience/', text: '发送日常，依次执行。', version: 'v1' }
  mock.call.mockReset().mockImplementation(async (_, action, values) => {
    if (action === 'audience.open') mock.saved.enabled = true
    if (action === 'audience.close') mock.saved.enabled = false
    if (action === 'audience.save') { mock.saved.text = values.text; mock.saved.version = 'v2' }
    return { ...mock.saved }
  })
})
afterEach(() => { wrapper?.unmount(); vi.restoreAllMocks() })
async function open() { wrapper = mount(AudienceSettings); await flushPromises(); return wrapper }
const button = text => wrapper.findAll('button').find(item => item.text() === text)

it('挂载只读取服务端状态，只有一个开关与文字编辑，没有旧显示选项', async () => {
  await open()
  expect(mock.call.mock.calls.map(call => call[1])).toEqual(['audience.read'])
  expect(wrapper.findAll('[role="switch"]')).toHaveLength(1)
  expect(wrapper.find('select').exists()).toBe(false)
  expect(wrapper.find('input[type="number"]').exists()).toBe(false)
  expect(wrapper.get('textarea').element.value).toBe(mock.saved.text)
  expect(wrapper.get('input[readonly]').element.value).toBe(mock.saved.url)
  expect(wrapper.text()).toContain('关闭不会停止互动')
  expect(button('保存文字').element.disabled).toBe(true)
})

it('启用只提供网页服务和URL，无打开页面入口，重新挂载恢复同一地址与开关', async () => {
  const popup = vi.spyOn(window, 'open')
  await open()
  await wrapper.get('[role="switch"]').setValue(true); await flushPromises()
  expect(mock.call).toHaveBeenCalledWith('gamer-live', 'audience.open', {})
  expect(popup).not.toHaveBeenCalled()
  const destination = wrapper.get('input[readonly]').element.value
  expect(destination).toBe(mock.saved.url)
  expect(new URL(destination).search).toBe('')
  expect(wrapper.find('a').exists()).toBe(false)
  expect(wrapper.text()).toContain('关闭不会停止互动')
  wrapper.unmount(); await open()
  expect(wrapper.get('[role="switch"]').element.checked).toBe(true)
  expect(wrapper.get('input[readonly]').element.value).toBe(destination)
  expect(mock.call.mock.calls.filter(call => call[1] === 'audience.open')).toHaveLength(1)
  await wrapper.get('[role="switch"]').setValue(false); await flushPromises()
  expect(wrapper.get('[role="switch"]').element.checked).toBe(false)
  expect(wrapper.get('input[readonly]').element.value).toBe(destination)
  expect(wrapper.text()).toContain('关闭不会停止互动')
  expect(mock.call.mock.calls.some(call => call[1].includes('queue'))).toBe(false)
})

it('保存原样保留换行和空串，携带服务端版本；开关不丢失文字草稿', async () => {
  await open()
  const draft = '第一行\n第二行 <img src=x onerror=alert(1)>'
  await wrapper.get('textarea').setValue(draft)
  await wrapper.get('[role="switch"]').setValue(true); await flushPromises()
  expect(wrapper.get('textarea').element.value).toBe(draft)
  await wrapper.get('form').trigger('submit'); await flushPromises()
  expect(mock.call).toHaveBeenCalledWith('gamer-live', 'audience.save', { text: draft, expected_version: 'v1' })
  expect(button('保存文字').element.disabled).toBe(true)
  await wrapper.get('textarea').setValue(''); await wrapper.get('form').trigger('submit'); await flushPromises()
  expect(mock.call).toHaveBeenCalledWith('gamer-live', 'audience.save', { text: '', expected_version: 'v2' })
})

it('开关失败回到已确认状态，保存冲突保留草稿', async () => {
  await open()
  mock.call.mockRejectedValueOnce(new Error('无法开启网页素材'))
  await wrapper.get('[role="switch"]').setValue(true); await flushPromises()
  expect(wrapper.get('[role="switch"]').element.checked).toBe(false)
  expect(wrapper.get('[role="alert"]').text()).toContain('无法开启网页素材')
  await wrapper.get('textarea').setValue('保留这份草稿')
  mock.call.mockRejectedValueOnce(new Error('设置已在其他页面修改'))
  await wrapper.get('form').trigger('submit'); await flushPromises()
  expect(wrapper.get('textarea').element.value).toBe('保留这份草稿')
  expect(wrapper.get('[role="alert"]').text()).toContain('设置已在其他页面修改')
})

it('读取失败不伪装成关闭，可以重试恢复', async () => {
  mock.call.mockRejectedValueOnce(new Error('服务暂不可用'))
  await open()
  expect(wrapper.get('[role="switch"]').element.disabled).toBe(true)
  expect(wrapper.find('textarea').exists()).toBe(false)
  await button('重新读取').trigger('click'); await flushPromises()
  expect(wrapper.get('[role="switch"]').element.disabled).toBe(false)
  expect(wrapper.get('textarea').element.value).toBe(mock.saved.text)
})

it('文字上限按Unicode字符计数，复制地址失败提供手动恢复方式', async () => {
  await open()
  await wrapper.get('textarea').setValue('😀'.repeat(1000))
  expect(wrapper.get('#audience-text-count').text()).toBe('1000 / 1000')
  expect(button('保存文字').element.disabled).toBe(false)
  await wrapper.get('textarea').setValue('😀'.repeat(1001))
  expect(button('保存文字').element.disabled).toBe(true)
  await wrapper.get('[role="switch"]').setValue(true); await flushPromises()
  vi.spyOn(navigator.clipboard, 'writeText').mockRejectedValueOnce(new Error('denied'))
  await button('复制地址').trigger('click'); await flushPromises()
  expect(wrapper.get('[role="alert"]').text()).toContain('选中地址后手动复制')
})
