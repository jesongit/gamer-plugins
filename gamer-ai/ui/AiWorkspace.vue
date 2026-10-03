<script setup>
import { computed, inject, ref, watch } from 'vue'
import { WORKSPACE_CONTEXT_KEY } from '../../../web/src/workspace/context'
import AgentConversation from './AgentConversation.vue'
import GameSessionPane from './GameSessionPane.vue'
import MemoryLibrary from './MemoryLibrary.vue'
import ServiceSettings from './ServiceSettings.vue'
const workspace = inject(WORKSPACE_CONTEXT_KEY, null)
const packageId = computed(() => workspace?.getSnapshot?.().currentPackageId || '')
const page = ref('chat'), attached = ref([]), chat = ref(null), settingsSection = ref(''), gameOptions = ref(null)
function openSettings(section = 'settings') { gameOptions.value = chat.value?.getGameOptions(); settingsSection.value = section; page.value = 'chat' }
async function settingsChanged() { await chat.value?.refreshSettings() }
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
    <nav aria-label="AI 工作台"><button v-for="(label,key) in {chat:'对话',memory:'记忆库',services:'可选服务'}" :key="key" :aria-current="page === key ? 'page' : undefined" @click="page=key">{{ label }}</button></nav>
    <AgentConversation ref="chat" v-show="page === 'chat'" :active="page === 'chat'" :package-id="packageId" :attached-memory="attached" @detach="attached = attached.filter(item => item.id !== $event)" @memory="page='memory'" @settings="openSettings" />
    <aside v-if="settingsSection" class="agent-settings-drawer" aria-label="Agent 设置"><div class="drawer-heading"><b>Agent 设置</b><button @click="settingsSection=''">关闭设置</button></div><GameSessionPane settings-only :initial-section="settingsSection" :initial-game-options="gameOptions" @settings-changed="settingsChanged" @game-options="chat?.setGameOptions($event)" /></aside>
    <MemoryLibrary v-if="page === 'memory'" :package-id="packageId" @attach="attach" />
    <ServiceSettings v-if="page === 'services'" />
  </div>
</template>
<style scoped>
.ai-agent-workspace{position:relative;display:flex;flex:1;flex-direction:column;min-width:0;min-height:0;overflow:hidden;color:var(--text-0,#edf0ee);background:var(--bg-1,#181b1c)}nav{display:flex;gap:5px;padding:7px 10px;border-bottom:1px solid var(--border,#41484a);flex-wrap:wrap}nav button{font:inherit;font-size:11px;color:var(--text-2,#aab5b2);background:transparent;border:1px solid transparent;border-radius:6px;padding:5px 10px;cursor:pointer}nav button[aria-current=page]{color:var(--text-0,#edf0ee);background:var(--bg-2,#282b2d)}nav button:focus-visible{outline:2px solid var(--accent,#e4c956);outline-offset:2px}
.agent-settings-drawer{position:absolute;inset:0 0 0 auto;width:min(440px,100%);max-width:100%;display:flex;flex-direction:column;background:var(--bg-1,#181b1c);border-left:1px solid var(--border,#41484a);box-shadow:-10px 0 30px #0003;z-index:3}.drawer-heading{display:flex;align-items:center;justify-content:space-between;padding:12px 14px;font-size:13px;border-bottom:1px solid var(--border,#41484a)}.drawer-heading button{font:inherit;background:transparent;color:var(--text-2,#aab5b2);border:0;cursor:pointer}
</style>
