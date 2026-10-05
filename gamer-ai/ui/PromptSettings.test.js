import { beforeEach, afterEach, expect, it, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
const mocks=vi.hoisted(()=>({call:vi.fn()}))
vi.mock('../../../web/src/api',()=>({api:{callExtension:mocks.call}}))
import PromptSettings from './PromptSettings.vue'
let wrappers, saved
const button=(wrapper,text)=>wrapper.findAll('button').find(button=>button.text()===text)
async function create(){const wrapper=mount(PromptSettings);wrappers.push(wrapper);await flushPromises();return wrapper}
beforeEach(()=>{
  wrappers=[];saved={version:'v1',chat_system_prompt:'对话系统默认',game_system_prompt:'游玩系统默认',import_system_prompt:'合并系统默认',defaults:{chat_system_prompt:'对话系统默认',game_system_prompt:'游玩系统默认',import_system_prompt:'合并系统默认'}}
  mocks.call.mockReset().mockImplementation(async(_,action,values)=>{
    if(action==='prompts.get')return structuredClone(saved)
    if(action==='prompts.save'){saved={...saved,...values,version:'v2'};delete saved.expected_version;return structuredClone(saved)}
    throw new Error(`Unexpected ${action}`)
  })
})
afterEach(()=>wrappers.forEach(wrapper=>wrapper.unmount()))
it('分别读取三种基础系统提示词，用独立版本全量保存，不调用模型或设备',async()=>{
  const wrapper=await create()
  expect(wrapper.findAll('textarea').map(field=>field.element.value)).toEqual(['对话系统默认','游玩系统默认','合并系统默认'])
  await wrapper.get('[aria-label="游玩基础系统提示词"]').setValue('先观察再行动\n需要结果复核')
  await wrapper.get('form').trigger('submit');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','prompts.save',{expected_version:'v1',chat_system_prompt:'对话系统默认',game_system_prompt:'先观察再行动\n需要结果复核',import_system_prompt:'合并系统默认'})
  expect(wrapper.emitted('saved')).toHaveLength(1)
  expect(wrapper.text()).toContain('下一次模型请求')
  expect(mocks.call.mock.calls.every(call=>['prompts.get','prompts.save'].includes(call[1]))).toBe(true)
})
it('恢复默认先填入表单，不丢失其他编辑或直接写入服务',async()=>{
  saved.chat_system_prompt='自定义对话'
  const wrapper=await create()
  await wrapper.get('[aria-label="游玩基础系统提示词"]').setValue('未保存的游玩草稿')
  await wrapper.findAll('.prompt-field button')[0].trigger('click')
  expect(wrapper.get('[aria-label="对话基础系统提示词"]').element.value).toBe('对话系统默认')
  expect(wrapper.get('[aria-label="游玩基础系统提示词"]').element.value).toBe('未保存的游玩草稿')
  expect(mocks.call.mock.calls).toHaveLength(1)
  expect(wrapper.text()).toContain('点击保存后生效')
})
it('中文按UTF-8字节校验32KiB，阻止超长提交而保留完整输入',async()=>{
  const wrapper=await create(),field=wrapper.get('[aria-label="对话基础系统提示词"]')
  const long='中'.repeat(10923)
  await field.setValue(long)
  expect(field.element.value).toBe(long)
  expect(button(wrapper,'保存提示词').element.disabled).toBe(true)
  await wrapper.get('form').trigger('submit');await flushPromises()
  expect(mocks.call.mock.calls.some(call=>call[1]==='prompts.save')).toBe(false)
  await field.setValue('中'.repeat(10922))
  expect(button(wrapper,'保存提示词').element.disabled).toBe(false)
})
it('版本冲突保留编辑和旧版本，显示错误供重新读取',async()=>{
  const wrapper=await create()
  await wrapper.get('[aria-label="对话基础系统提示词"]').setValue('保留我的编辑')
  mocks.call.mockImplementation(async(_,action)=>{if(action==='prompts.save')throw new Error('提示词版本冲突');return structuredClone(saved)})
  await wrapper.get('form').trigger('submit');await flushPromises()
  expect(wrapper.get('[role="alert"]').text()).toContain('版本冲突')
  expect(wrapper.get('[aria-label="对话基础系统提示词"]').element.value).toBe('保留我的编辑')
  expect(wrapper.emitted('saved')).toBeUndefined()
})
it.each(['','  \n\t'])('空或仅空白提示词%j拒绝提交，恢复默认后可保存',async value=>{
  const wrapper=await create()
  await wrapper.get('[aria-label="对话基础系统提示词"]').setValue(value)
  expect(button(wrapper,'保存提示词').element.disabled).toBe(true)
  await wrapper.get('form').trigger('submit');await flushPromises()
  expect(mocks.call.mock.calls.some(call=>call[1]==='prompts.save')).toBe(false)
  await wrapper.findAll('.prompt-field button')[0].trigger('click')
  expect(wrapper.get('[aria-label="对话基础系统提示词"]').element.value).toBe('对话系统默认')
})
