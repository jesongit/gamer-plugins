<script setup>
import { computed, inject, ref, watch } from 'vue'
import { pluginMessageChannel } from '../../../web/src/workspace/plugin-messages'
import { api } from '../../../web/src/api'
import { WORKSPACE_CONTEXT_KEY } from '../../../web/src/workspace/context'
import AgentConversation from './AgentConversation.vue'
import GameSessionPane from './GameSessionPane.vue'
import MemoryLibrary from './MemoryLibrary.vue'
import ServiceSettings from './ServiceSettings.vue'
const workspace = inject(WORKSPACE_CONTEXT_KEY, null)
const packageId = computed(() => workspace?.getSnapshot?.().currentPackageId || '')
const automationRequest = pluginMessageChannel('gamer-ai:automation-context')
const automation = ref(null), automationError = ref(''), automationScripts = ref([]), attachScript = ref('')
let attachmentGeneration = 0
async function refreshAutomations() {
  const request = ++attachmentGeneration, pkg = packageId.value
  if (!pkg) return
  try { const scripts = await api.listScripts(pkg); if (request === attachmentGeneration && pkg === packageId.value) automationScripts.value = scripts || [] } catch (e) { if (request === attachmentGeneration) automationError.value = e.message }
}
function attachAutomation() {
  if (!attachScript.value || !automationScripts.value.some(script => script.id === attachScript.value)) return
  automation.value = { script_id: attachScript.value.slice(packageId.value.length + 1) }; automationError.value = ''; page.value = 'chat'
}
const page = ref('chat'), attached = ref([]), chat = ref(null), settingsSection = ref(''), gameOptions = ref(null)
watch(() => automationRequest.seq, () => {
  if (automationRequest.seq <= (automationRequest.consumedSeq || 0) || automationRequest.packageId !== packageId.value || !automationRequest.automation?.script_id) return
  automationRequest.consumedSeq = automationRequest.seq
  automation.value = { ...automationRequest.automation }; page.value = 'chat'
}, { immediate: true })

function openSettings(section = 'settings') { gameOptions.value = chat.value?.getGameOptions(); settingsSection.value = section; page.value = 'chat' }
async function settingsChanged() { await chat.value?.refreshSettings() }
async function externalSessionStarted(value) {
  if(!value?.conversation_id) return
  page.value='chat'
  await chat.value?.refreshList()
  await chat.value?.select(value.conversation_id)
}
function attach(item) {
  const existing = attached.value.findIndex(memory => memory.id === item.id)
  if (existing >= 0) attached.value.splice(existing, 1, item)
  else attached.value.push(item)
  page.value = 'chat'
}
watch(packageId, () => { attached.value = []; automation.value = null; automationScripts.value = []; attachScript.value = ''; automationError.value = ''; attachmentGeneration++ })
</script>
<template>
  <div class="ai-agent-workspace">
    <nav aria-label="AI 工作台"><button v-for="(label,key) in {chat:'对话',memory:'记忆库',services:'可选服务'}" :key="key" :aria-current="page === key ? 'page' : undefined" @click="page=key">{{ label }}</button></nav>
    <details class="automation-attachment"><summary @click="refreshAutomations">附加自动化或运行记录</summary><p>先读取所选脚本与运行证据再分析；修改只形成候选，正式保存需要你确认。不会控制设备。</p><div><select v-model="attachScript" aria-label="选择自动化上下文"><option value="">选择脚本…</option><option v-for="script in automationScripts" :key="script.id" :value="script.id">{{ script.name || script.id }}</option></select><button :disabled="!attachScript" @click="attachAutomation">附加</button></div><p v-if="automationError" role="alert">{{ automationError }}</p></details>
    <AgentConversation ref="chat" v-show="page === 'chat'" :active="page === 'chat'" :package-id="packageId" :attached-memory="attached" :automation-context="automation" @detach-automation="automation = null" @detach="attached = attached.filter(item => item.id !== $event)" @memory="page='memory'" @settings="openSettings" />
    <aside v-if="settingsSection" class="agent-settings-drawer" aria-label="Agent 设置"><div class="drawer-heading"><b>Agent 设置</b><button @click="settingsSection=''">关闭设置</button></div><GameSessionPane settings-only :initial-section="settingsSection" :initial-game-options="gameOptions" @settings-changed="settingsChanged" @game-options="chat?.setGameOptions($event)" @session-start="externalSessionStarted" /></aside>
    <MemoryLibrary v-if="page === 'memory'" :package-id="packageId" @attach="attach" @settings="openSettings" />
    <ServiceSettings v-if="page === 'services'" />
  </div>
</template>
<style scoped>
.automation-attachment{margin:4px 10px;padding:7px;font-size:11px;border:1px solid var(--border);border-radius:4px}.automation-attachment summary{cursor:pointer}.automation-attachment p{line-height:1.6;color:var(--text-2)}.automation-attachment div{display:flex;gap:6px}.automation-attachment select{flex:1;min-width:0;background:var(--bg-0);color:var(--text-0);border:1px solid var(--border)}.automation-attachment button{background:var(--bg-2);color:var(--text-0);border:1px solid var(--border);cursor:pointer}.ai-agent-workspace{position:relative;display:flex;flex:1;flex-direction:column;min-width:0;min-height:0;overflow:hidden;color:var(--text-0,#edf0ee);background:var(--bg-1,#181b1c)}nav{display:flex;gap:5px;padding:7px 10px;border-bottom:1px solid var(--border,#41484a);flex-wrap:wrap}nav button{font:inherit;font-size:11px;color:var(--text-2,#aab5b2);background:transparent;border:1px solid transparent;border-radius:6px;padding:5px 10px;cursor:pointer}nav button[aria-current=page]{color:var(--text-0,#edf0ee);background:var(--bg-2,#282b2d)}nav button:focus-visible{outline:2px solid var(--accent,#e4c956);outline-offset:2px}
.agent-settings-drawer{position:absolute;inset:0 0 0 auto;width:min(440px,100%);max-width:100%;display:flex;flex-direction:column;background:var(--bg-1,#181b1c);border-left:1px solid var(--border,#41484a);box-shadow:-10px 0 30px #0003;z-index:3}.drawer-heading{display:flex;align-items:center;justify-content:space-between;padding:12px 14px;font-size:13px;border-bottom:1px solid var(--border,#41484a)}.drawer-heading button{font:inherit;background:transparent;color:var(--text-2,#aab5b2);border:0;cursor:pointer}
</style>
