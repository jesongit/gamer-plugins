<template>
  <section class="automation-workbench">
    <nav class="workbench-tabs" aria-label="自动化子页签">
      <button v-for="tab in tabs" :key="tab.key" type="button" class="tab-btn"
        :class="{ active: activeTab === tab.key }" :aria-pressed="activeTab === tab.key"
        :disabled="switching" @click="selectTab(tab.key)">{{ tab.label }}</button>
    </nav>
    <SourceYamlEditor ref="editor" v-if="contentTab === 'scripts'" v-show="activeTab === 'scripts'" :context="ctx.scripts" @show-logs="openLogs" @generate="selectTab('generation')" />
    <AutomationGenerationPanel ref="generationEditor" v-if="contentTab === 'generation'" v-show="activeTab === 'generation'" :package-id="ctx.scripts.packageId" @editing-state="ctx.scripts.sourceEditorDirty = $event.dirty; ctx.scripts.sourceEditorSaving = $event.saving" @show-logs="openLogs" />
    <AutomationCandidateReview ref="historyEditor" v-if="contentTab === 'history'" v-show="activeTab === 'history'" :package-id="ctx.scripts.packageId" />
    <div v-if="contentTab === 'templates'" v-show="activeTab === 'templates'" class="workbench-templates"><TemplateCapture :context="ctx.templates" /></div>
    <RunDetails v-if="logsMounted" v-show="activeTab === 'logs'" class="workbench-logs" :visible="activeTab === 'logs'" :device-id="logDevice" :target="logTarget" :live-run="logRun" @edit="selectTab(lastContentTab)" @locate="runLocation = $event" />
    <RunErrorLocation v-if="runLocation" :event="runLocation" @close="runLocation = null" />
  </section>
</template>

<script setup>
import { computed, reactive, ref, watch } from 'vue'
import SourceYamlEditor from './SourceYamlEditor.vue'
import AutomationGenerationPanel from './AutomationGenerationPanel.vue'
import AutomationCandidateReview from './AutomationCandidateReview.vue'
import { pluginMessageChannel } from '../../../../../../web/src/workspace/plugin-messages'
import TemplateCapture from './TemplateCapture.vue'
import RunDetails from './RunDetails.vue'
import RunErrorLocation from './RunErrorLocation.vue'
import { api } from '../../../../../../web/src/api'
import { readAuthoringLocation, rememberAuthoringLocation } from './generation-state'

const props = defineProps({ context: { type: Object, required: true } })
const ctx = reactive(props.context)
const tabs = [{ key: 'scripts', label: '脚本库' }, { key: 'templates', label: '模板' }, { key: 'generation', label: '工作稿与版本' }, { key: 'history', label: '历史候选' }, { key: 'logs', label: '运行日志' }]
const activeTab = computed(() => ctx.scripts.automationTab)
const lastContentTab = ref(activeTab.value === 'logs' ? 'scripts' : activeTab.value)
const contentTab = computed(() => activeTab.value === 'logs' ? lastContentTab.value : activeTab.value)
const logsMounted = ref(false), logRequest = ref(null), runLocation = ref(null)
const logDevice = computed(() => ctx.scripts.store.deviceId || '')
const requestedLog = computed(() => logRequest.value?.deviceId === logDevice.value ? logRequest.value : null)
const logRun = computed(() => ctx.scripts.store.runId || requestedLog.value?.runId || '')
const logTarget = computed(() => requestedLog.value?.target || ctx.scripts.selScript || '')
const reviewRequest = pluginMessageChannel('gamer-yaml:open-generation')
const editor = ref(null), generationEditor = ref(null), historyEditor = ref(null), switching = ref(false)
let restoreRequest = 0
async function selectTab(key, fromRequest = false) {
  if (switching.value || key === activeTab.value && (key !== 'generation' || fromRequest || !reviewRequest.candidateId)) return
  restoreRequest++
  const previous = activeTab.value
  switching.value = true
  try {
    // Reading logs keeps the active editor mounted, including unsaved source and private runs.
    if (key !== 'logs' && (key !== contentTab.value || key === 'generation' && reviewRequest.candidateId && !fromRequest)) {
      if (contentTab.value === 'scripts' && !await editor.value?.beforeTabChange()) return
      if (contentTab.value === 'generation' && !await generationEditor.value?.beforeTabChange()) return
      if (contentTab.value === 'history' && !await historyEditor.value?.beforeTabChange()) return
    }
    if (key === 'generation' && !fromRequest) reviewRequest.candidateId = null
    if (activeTab.value === previous) ctx.scripts.automationTab = key
  } finally { switching.value = false }
}
function openLogs(request) {
  if (request?.runId) logRequest.value = request
  void selectTab('logs')
}
watch(() => ctx.scripts.packageId, async pkg => {
  const token = ++restoreRequest
  if (!pkg || ctx.scripts.sourceEditorDirty || ctx.scripts.sourceEditorSaving) return
  const saved = readAuthoringLocation(pkg).tab
  if (tabs.some(tab => tab.key === saved)) { ctx.scripts.automationTab = saved; return }
  if (activeTab.value !== 'scripts') return
  try {
    const result = await api.callExtension('gamer-yaml', 'authoring.list', { package_id: pkg })
    const scripts = result?.scripts || []
    // A private draft script remains accessible after the first reload, even
    // before a viewing preference has been saved. This never publishes it.
    if (token === restoreRequest && pkg === ctx.scripts.packageId && activeTab.value === 'scripts'
      && !ctx.scripts.sourceEditorDirty && !ctx.scripts.sourceEditorSaving
      && scripts.some(script => script.head_version) && !scripts.some(script => script.published_version)) {
      ctx.scripts.automationTab = 'generation'
    }
  } catch { /* Keep the normal script-library entry when discovery is unavailable. */ }
}, { immediate: true })
watch(activeTab, key => { if (key === 'logs') logsMounted.value = true; else lastContentTab.value = key; rememberAuthoringLocation(ctx.scripts.packageId, { tab: key }) }, { immediate: true, flush: 'sync' })
watch(() => reviewRequest.seq, () => { if (reviewRequest.seq && reviewRequest.packageId === ctx.scripts.packageId) void selectTab(reviewRequest.candidateId ? 'history' : 'generation', true) }, { immediate: true })
</script>

<style scoped>
.automation-workbench{display:flex;flex-direction:column;flex:1;min-height:0;min-width:0;gap:8px;padding:var(--workspace-inset,8px);box-sizing:border-box;overflow:hidden}
.workbench-tabs{display:flex;flex-wrap:wrap;gap:4px;flex-shrink:0;border-bottom:1px solid var(--border);padding-bottom:6px}
.workbench-logs{padding:2px;box-sizing:border-box}
.workbench-templates{display:flex;flex:1;min-height:0;min-width:0;flex-direction:column}
.automation-workbench :deep(.authoring),.automation-workbench :deep(.generation-panel){padding:2px}
.tab-btn{height:28px;padding:3px 10px;border:1px solid transparent;border-radius:3px;background:transparent;color:var(--text-2);font-size:13px;cursor:pointer}
.tab-btn:hover{color:var(--text-0)}
.tab-btn.active{border-color:var(--border);background:var(--bg-2);color:var(--text-0);font-weight:700}
.tab-btn:disabled{opacity:.5;cursor:default}
</style>
