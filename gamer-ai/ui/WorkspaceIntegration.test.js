import { expect, it, vi } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import { defineComponent, reactive } from 'vue'
import { WORKSPACE_CONTEXT_KEY } from '../../../web/src/workspace/context'
import AiWorkspace from './AiWorkspace.vue'

it('开始游玩选择统一对话，附记忆固定修订，切包清除待附引用', async () => {
  const select=vi.fn(),refresh=vi.fn(),context=reactive({currentPackageId:'default'})
  const Chat=defineComponent({name:'AgentConversation',props:['packageId','attachedMemory','active'],setup(_, {expose}){expose({select,refreshList:refresh})},template:'<section>统一聊天</section>'})
  const Game=defineComponent({name:'GameSessionPane',emits:['session-start'],template:'<button @click="$emit(\'session-start\',{conversation_id:\'game-1\'})">模拟已建立游戏会话</button>'})
  const Memory=defineComponent({name:'MemoryLibrary',emits:['attach'],template:'<button @click="$emit(\'attach\',{id:\'m1\',revision:7,title:\'背包经验\',content_package:\'default\'})">附选中的记忆</button>'})
  const wrapper=mount(AiWorkspace,{global:{provide:{[WORKSPACE_CONTEXT_KEY]:{getSnapshot:()=>context}},stubs:{AgentConversation:Chat,GameSessionPane:Game,MemoryLibrary:Memory,ServiceSettings:true}}})
  try {
    const button=label=>wrapper.findAll('button').find(item=>item.text()===label)
    await button('游玩控制').trigger('click')
    await button('模拟已建立游戏会话').trigger('click');await flushPromises()
    expect(refresh).toHaveBeenCalledOnce();expect(select).toHaveBeenCalledWith('game-1')
    expect(wrapper.get('nav button[aria-current="page"]').text()).toBe('对话')
    await button('记忆库').trigger('click');await button('附选中的记忆').trigger('click');await flushPromises()
    expect(wrapper.findComponent(Chat).props('attachedMemory')).toEqual([{id:'m1',revision:7,title:'背包经验',content_package:'default'}])
    context.currentPackageId='other';await flushPromises()
    expect(wrapper.findComponent(Chat).props('attachedMemory')).toEqual([])
    expect(wrapper.findComponent(Chat).props('packageId')).toBe('other')
  } finally {wrapper.unmount()}
})
