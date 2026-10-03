import { expect, it, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import { defineComponent, reactive } from 'vue'
import { WORKSPACE_CONTEXT_KEY } from '../../../web/src/workspace/context'
import AiWorkspace from './AiWorkspace.vue'

it('只有主对话，设置抽屉不会另建游玩输入，记忆引用与包切换保留统一入口', async () => {
  const refresh=vi.fn(),options=vi.fn(),context=reactive({currentPackageId:'default'})
  const Chat=defineComponent({name:'AgentConversation',props:['packageId','attachedMemory','active'],emits:['settings'],setup(_, {expose}){expose({refreshSettings:refresh,setGameOptions:options,getGameOptions:()=>({mode:'api',limits:{max_tokens:0}})})},template:'<section><button @click="$emit(\'settings\',\'settings\')">模型设置</button><textarea aria-label="统一消息" /></section>'})
  const Settings=defineComponent({name:'GameSessionPane',props:{settingsOnly:Boolean,initialSection:String,initialGameOptions:Object},emits:['settings-changed','game-options'],template:'<section><button @click="$emit(\'settings-changed\')">模拟已保存设置</button><button @click="$emit(\'game-options\',{limits:{max_tokens:0}})">同步预算</button></section>'})
  const Memory=defineComponent({name:'MemoryLibrary',emits:['attach'],template:'<button @click="$emit(\'attach\',{id:\'m1\',revision:7,title:\'背包经验\',content_package:\'default\'})">附选中的记忆</button>'})
  const wrapper=mount(AiWorkspace,{global:{provide:{[WORKSPACE_CONTEXT_KEY]:{getSnapshot:()=>context}},stubs:{AgentConversation:Chat,GameSessionPane:Settings,MemoryLibrary:Memory,ServiceSettings:true}}})
  try {
    const button=label=>wrapper.findAll('button').find(item=>item.text()===label)
    expect(wrapper.findAll('nav button').map(item=>item.text())).toEqual(['对话','记忆库','可选服务'])
    await button('模型设置').trigger('click');await flushPromises()
    expect(wrapper.findComponent(Settings).props()).toMatchObject({settingsOnly:true,initialSection:'settings',initialGameOptions:{mode:'api',limits:{max_tokens:0}}})
    expect(wrapper.findAll('textarea')).toHaveLength(1)
    await button('模拟已保存设置').trigger('click');await flushPromises();expect(refresh).toHaveBeenCalledOnce()
    await button('同步预算').trigger('click');expect(options).toHaveBeenCalledWith({limits:{max_tokens:0}})
    await button('关闭设置').trigger('click');expect(wrapper.findComponent(Settings).exists()).toBe(false)
    await button('记忆库').trigger('click');await button('附选中的记忆').trigger('click');await flushPromises()
    expect(wrapper.get('nav button[aria-current="page"]').text()).toBe('对话')
    expect(wrapper.findComponent(Chat).props('attachedMemory')).toEqual([{id:'m1',revision:7,title:'背包经验',content_package:'default'}])
    context.currentPackageId='other';await flushPromises()
    expect(wrapper.findComponent(Chat).props('attachedMemory')).toEqual([])
    expect(wrapper.findComponent(Chat).props('packageId')).toBe('other')
  } finally {wrapper.unmount()}
})
