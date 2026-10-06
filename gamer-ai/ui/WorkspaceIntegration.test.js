import { expect, it, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import { defineComponent, reactive } from 'vue'
import { WORKSPACE_CONTEXT_KEY } from '../../../web/src/workspace/context'
import AiWorkspace from './AiWorkspace.vue'

it('只有主对话，设置抽屉不会另建游玩输入，记忆引用与包切换保留统一入口', async () => {
  const refresh=vi.fn(),list=vi.fn(),select=vi.fn(),options=vi.fn(),context=reactive({currentPackageId:'default'})
  const Chat=defineComponent({name:'AgentConversation',props:['packageId','attachedMemory','active'],emits:['settings'],setup(_, {expose}){expose({refreshSettings:refresh,refreshList:list,select,setGameOptions:options,getGameOptions:()=>({limits:{max_tokens:0}})})},template:'<section><button @click="$emit(\'settings\',\'settings\')">模型设置</button><textarea aria-label="统一消息" /></section>'})
  const Settings=defineComponent({name:'GameSessionPane',props:{settingsOnly:Boolean,initialSection:String,initialGameOptions:Object},emits:['settings-changed','game-options','session-start'],template:'<section><button @click="$emit(\'settings-changed\')">模拟已保存设置</button><button @click="$emit(\'game-options\',{limits:{max_tokens:0}})">同步预算</button><button @click="$emit(\'session-start\',{conversation_id:\'external\'})">模拟外部会话已建立</button></section>'})
  const Memory=defineComponent({name:'MemoryLibrary',emits:['attach'],template:'<button @click="$emit(\'attach\',{id:\'m1\',revision:7,title:\'背包经验\',content_package:\'default\'})">附选中的记忆</button>'})
  const wrapper=mount(AiWorkspace,{global:{provide:{[WORKSPACE_CONTEXT_KEY]:{getSnapshot:()=>context}},stubs:{AgentConversation:Chat,GameSessionPane:Settings,MemoryLibrary:Memory,ServiceSettings:true}}})
  try {
    const button=label=>wrapper.findAll('button').find(item=>item.text()===label)
    expect(wrapper.findAll('nav button').map(item=>item.text())).toEqual(['对话','记忆库','可选服务'])
    await button('模型设置').trigger('click');await flushPromises()
    expect(wrapper.findComponent(Settings).props()).toMatchObject({settingsOnly:true,initialSection:'settings',initialGameOptions:{limits:{max_tokens:0}}})
    expect(wrapper.findAll('textarea')).toHaveLength(1)
    await button('模拟已保存设置').trigger('click');await flushPromises();expect(refresh).toHaveBeenCalledOnce()
    await button('同步预算').trigger('click');expect(options).toHaveBeenCalledWith({limits:{max_tokens:0}})
    await button('模拟外部会话已建立').trigger('click');await flushPromises()
    expect(list).toHaveBeenCalledOnce();expect(select).toHaveBeenCalledWith('external')
    expect(wrapper.findComponent(Settings).exists()).toBe(true)
    await button('关闭设置').trigger('click');expect(wrapper.findComponent(Settings).exists()).toBe(false)
    await button('记忆库').trigger('click');await button('附选中的记忆').trigger('click');await flushPromises()
    expect(wrapper.get('nav button[aria-current="page"]').text()).toBe('对话')
    expect(wrapper.findComponent(Chat).props('attachedMemory')).toEqual([{id:'m1',revision:7,title:'背包经验',content_package:'default'}])
    context.currentPackageId='other';await flushPromises()
    expect(wrapper.findComponent(Chat).props('attachedMemory')).toEqual([])
    expect(wrapper.findComponent(Chat).props('packageId')).toBe('other')
  } finally {wrapper.unmount()}
})

it('自动化导航只附加同配置包引用，包切换立即清除', async () => {
  const {pluginMessageChannel}=await import('../../../web/src/workspace/plugin-messages')
  const request=pluginMessageChannel('gamer-ai:automation-context'),context=reactive({currentPackageId:'default'})
  const Chat=defineComponent({name:'AgentConversation',props:['automationContext','packageId'],template:'<section />'})
  Object.assign(request,{packageId:'default',automation:{script_id:'daily.yaml',run_id:'run-1'},seq:request.seq+1})
  const wrapper=mount(AiWorkspace,{global:{provide:{[WORKSPACE_CONTEXT_KEY]:{getSnapshot:()=>context}},stubs:{AgentConversation:Chat,GameSessionPane:true,MemoryLibrary:true,ServiceSettings:true}}})
  try {
    expect(wrapper.findComponent(Chat).props('automationContext')).toEqual({script_id:'daily.yaml',run_id:'run-1'})
    Object.assign(request,{packageId:'foreign',automation:{script_id:'secret.yaml'},seq:request.seq+1});await flushPromises()
    expect(wrapper.findComponent(Chat).props('automationContext').script_id).toBe('daily.yaml')
    context.currentPackageId='other';await flushPromises();expect(wrapper.findComponent(Chat).props('automationContext')).toBe(null)
  } finally {wrapper.unmount();Object.assign(request,{packageId:'',automation:null,seq:request.seq+1})}
})
