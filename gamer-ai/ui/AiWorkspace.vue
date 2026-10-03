<script setup>
import { computed, inject, ref, watch } from 'vue'
import { WORKSPACE_CONTEXT_KEY } from '../../../web/src/workspace/context'
import AgentConversation from './AgentConversation.vue'
import GameSessionPane from './GameSessionPane.vue'
import MemoryLibrary from './MemoryLibrary.vue'
import ServiceSettings from './ServiceSettings.vue'
const workspace = inject(WORKSPACE_CONTEXT_KEY, null)
const packageId = computed(() => workspace?.getSnapshot?.().currentPackageId || '')
const page = ref('chat'), attached = ref([]), chat = ref(null)
async function showGameConversation(value) {page.value='chat';await chat.value?.refreshList();await chat.value?.select(value.conversation_id)}
function attach(item) {
  const existing = attached.value.findIndex(memory => memory.id === item.id)
  if (existing >= 0) attached.value.splice(existing, 1, item)
  else attached.value.push(item)
  page.value = 'chat'
}
watch(packageId, () => { attached.value = [] })
</script>
<template>
  <div class="ai-agent-workspace">
    <nav aria-label="AI 工作台"><button v-for="(label,key) in {chat:'对话',game:'游玩控制',memory:'记忆库',services:'可选服务'}" :key="key" :aria-current="page === key ? 'page' : undefined" @click="page=key">{{ label }}</button></nav>
    <AgentConversation ref="chat" v-show="page === 'chat'" :active="page === 'chat'" :package-id="packageId" :attached-memory="attached" @detach="attached = attached.filter(item => item.id !== $event)" @memory="page='memory'" @game="page='game'" />
    <GameSessionPane v-show="page === 'game'" @session-start="showGameConversation" />
    <MemoryLibrary v-if="page === 'memory'" :package-id="packageId" @attach="attach" />
    <ServiceSettings v-if="page === 'services'" />
  </div>
</template>
<style scoped>
.ai-agent-workspace{display:flex;flex:1;flex-direction:column;min-width:0;min-height:0;overflow:hidden;color:var(--text-0,#edf0ee);background:var(--bg-1,#181b1c)}nav{display:flex;gap:5px;padding:7px 10px;border-bottom:1px solid var(--border,#41484a);flex-wrap:wrap}nav button{font:inherit;font-size:11px;color:var(--text-2,#aab5b2);background:transparent;border:1px solid transparent;border-radius:4px;padding:5px 8px;cursor:pointer}nav button[aria-current=page]{color:var(--accent,#e4c956);border-color:var(--border,#41484a)}nav button:focus-visible{outline:2px solid var(--accent,#e4c956);outline-offset:2px}
</style>
