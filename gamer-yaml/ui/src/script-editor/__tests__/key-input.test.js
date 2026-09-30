// @vitest-environment happy-dom
import { afterEach, expect, it, vi } from 'vitest'
import { mount } from '@vue/test-utils'
import CellEditor from '../components/CellEditor.vue'
import { isKnownKey } from '../schema'

let wrapper
afterEach(() => { wrapper?.unmount(); wrapper = null })
function setup() {
  wrapper = mount(CellEditor, { attachTo: document.body, props: { cell: { lit: 'BACK' }, type: 'key', label: '按键' } })
  return wrapper.get('input[role="combobox"]')
}
it('使用主题列表搜索和键盘选择，保留直接输入 Android 数字键码', async () => {
  const input = setup()
  expect(wrapper.find('datalist').exists()).toBe(false)
  await input.setValue('Arrow')
  await wrapper.setProps({ cell: { lit: 'Arrow' } })
  expect(document.querySelectorAll('[role="option"]')).toHaveLength(4)
  await input.trigger('keydown', { key: 'ArrowDown' })
  await input.trigger('keydown', { key: 'Enter' })
  expect(wrapper.emitted('change').at(-1)[0]).toEqual({ lit: 'ArrowUp' })
  expect(document.querySelector('[role="listbox"]')).toBeNull()
  await input.setValue('1234')
  expect(wrapper.emitted('change').at(-1)[0]).toEqual({ lit: '1234' })
})
it.each([
  ['w', 'KeyW'], ['1', 'Digit1'], [' ', 'Space'], ['Escape', 'Escape'], ['ArrowLeft', 'ArrowLeft'],
  ['Shift', 'ShiftLeft'], ['Enter', 'Enter'], ['Tab', 'Tab'], ['F2', 'F2'], [';', 'Semicolon'],
])('点击后自动聚焦并只录入一次 %s (%s)，不触发快捷键', async (key, code) => {
  const input = setup()
  const bubble = vi.fn()
  document.addEventListener('keydown', bubble)
  try {
    await wrapper.get('.key-record').trigger('click')
    expect(document.activeElement).toBe(input.element)
    expect(input.attributes('readonly')).toBeDefined()
    expect(document.querySelector('[role="listbox"]')).toBeNull()
    await input.trigger('keydown', { key, code })
    await input.trigger('keydown', { key, code, repeat: true })
    await input.trigger('keyup', { key, code })
    expect(wrapper.emitted('change')).toEqual([[{ lit: code }]])
    expect(isKnownKey(code)).toBe(true)
    expect(bubble).not.toHaveBeenCalled()
    expect(wrapper.get('.key-record').text()).toBe('录入按键')
  } finally { document.removeEventListener('keydown', bubble) }
})
it('取消、失焦和输入法组合不改原值，离开后清理弹层', async () => {
  const input = setup()
  await wrapper.get('.key-record').trigger('click')
  await input.trigger('keydown', { key: 'Process', code: 'KeyA', isComposing: true })
  expect(wrapper.emitted('change')).toBeUndefined()
  await wrapper.get('.key-record').trigger('click')
  expect(wrapper.get('.key-record').text()).toBe('录入按键')
  await wrapper.get('.key-record').trigger('click')
  await input.trigger('blur')
  expect(wrapper.get('.key-record').text()).toBe('录入按键')
  expect(wrapper.emitted('change')).toBeUndefined()
  await wrapper.get('.combo-toggle').trigger('click')
  expect(document.querySelector('[role="listbox"]')).not.toBeNull()
  wrapper.unmount(); wrapper = null
  expect(document.querySelector('[role="listbox"]')).toBeNull()
})
