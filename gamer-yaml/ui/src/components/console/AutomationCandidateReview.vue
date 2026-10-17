<script setup>
import { onBeforeUnmount, ref, watch } from 'vue'
import { api } from '../../../../../../web/src/api'
import { pluginMessageChannel } from '../../../../../../web/src/workspace/plugin-messages'
import CandidateTemplatePreview from './CandidateTemplatePreview.vue'
const props = defineProps({ packageId: { type: String, default: '' } })
const emit = defineEmits(['editing-state'])
const request = pluginMessageChannel('gamer-yaml:open-generation')
const candidates = ref([]), candidate = ref(null), selected = ref(''), loading = ref(false), error = ref(''), note = ref(''), newName = ref('')
let epoch = 0
const call = (action, values = {}) => api.callExtension('gamer-yaml', action, { package_id: props.packageId, ...values })
async function load(id) {
  const token = ++epoch; selected.value = id; candidate.value = null; error.value = ''
  if (!id) return
  loading.value = true
  try { const result = await call('generation.get', { candidate_id: id }); if (token === epoch) candidate.value = result.candidate }
  catch (e) { if (token === epoch) error.value = e.message }
  finally { if (token === epoch) loading.value = false }
}
async function refresh() {
  const token = ++epoch; error.value = ''; loading.value = true
  try {
    const result = await call('generation.list')
    if (token !== epoch) return
    candidates.value = result.candidates || []
    const id = request.packageId === props.packageId && request.candidateId || selected.value
    if (id && candidates.value.some(v => v.id === id)) await load(id)
  } catch (e) { if (token === epoch) error.value = e.message }
  finally { if (token === epoch) loading.value = false }
}
function exportRecord() {
  if (!candidate.value) return
  const blob = new Blob([JSON.stringify(candidate.value, null, 2)], { type: 'application/json' })
  const url = URL.createObjectURL(blob), link = document.createElement('a')
  link.href = url; link.download = `${candidate.value.id}.json`; link.click(); setTimeout(() => URL.revokeObjectURL(url), 0)
  note.value = '已导出候选记录 JSON。完整原文件与冻结快照可由 Agent 通过 MCP 分块导出。'
}
async function importDraft() {
  if (!candidate.value || !newName.value.trim() || loading.value) return
  const token = epoch; loading.value = true; error.value = ''
  try {
    const result = await call('generation.import_draft', { candidate_id: candidate.value.id, new_script_id: newName.value.trim() })
    if (token !== epoch) return
    request.packageId = props.packageId; request.scriptId = result.script.script_id; request.candidateId = null; request.repairRun = null; request.seq++
    note.value = '已导入为新的私有工作稿，旧候选和正式脚本保留。'
  } catch (e) { if (token === epoch) error.value = e.message }
  finally { if (token === epoch) loading.value = false }
}
watch(() => props.packageId, () => { epoch++; selected.value = ''; candidate.value = null; void refresh() }, { immediate: true })
watch(() => request.candidateId, id => { if (id && request.packageId === props.packageId) void load(id) })
onBeforeUnmount(() => { epoch++; emit('editing-state', { dirty: false, saving: false }) })
defineExpose({ beforeTabChange: async () => !loading.value })
</script>
<template>
  <section class="legacy-candidates" aria-label="旧候选历史">
    <p class="hint">旧候选与冻结内容原样保留。这里可读取记录，或明确导入为新的工作稿；导入后由你检查、测试和发布。</p>
    <div class="toolbar"><label>候选记录<select :value="selected" :disabled="loading" @change="load($event.target.value)"><option value="">选择历史候选</option><option v-for="item in candidates" :key="item.id" :value="item.id">{{ item.name || item.id }} · {{ item.id }}</option></select></label><button class="btn" :disabled="loading" @click="refresh">刷新历史</button></div>
    <p v-if="loading" role="status">正在读取历史记录…</p><p v-if="error" class="error" role="alert">{{ error }}</p><p v-if="note" role="status">{{ note }}</p>
    <p v-if="!candidates.length && !loading" class="hint">当前配置包没有旧候选记录。</p>
    <template v-if="candidate"><div class="toolbar"><b>{{ candidate.name }} · 修订 {{ candidate.revision }}</b><button class="btn" @click="exportRecord">导出候选记录 JSON</button></div><pre class="source">{{ candidate.yaml }}</pre><details><summary>候选记录与历史用量</summary><pre>{{ JSON.stringify(candidate, null, 2) }}</pre></details><div v-for="item in candidate.templates || []" :key="item.name"><span>{{ item.name }}</span><CandidateTemplatePreview :package-id="packageId" :candidate-id="candidate.id" :revision="candidate.revision" :name="item.name" /></div><form class="toolbar" @submit.prevent="importDraft"><label>新工作稿名称<input v-model="newName" required placeholder="例如：重新整理.yaml" :disabled="loading" /></label><button class="btn btn-primary" :disabled="loading || !newName.trim()">导入为新工作稿</button></form><p class="hint">必须使用未占用的新名称；导入不会覆盖旧工作稿或正式脚本。</p></template>
  </section>
</template>
<style scoped>
.legacy-candidates{display:flex;flex-direction:column;gap:12px;overflow:auto;padding:8px;font-size:13px}.toolbar{display:flex;align-items:center;gap:8px;flex-wrap:wrap}.toolbar label{display:flex;gap:8px;align-items:center;flex:1;min-width:180px}input,select{min-width:0;flex:1;background:var(--bg-0);color:var(--text-0);border:1px solid var(--border);padding:8px}pre{white-space:pre-wrap;overflow-wrap:anywhere;max-height:360px;overflow:auto;font-family:var(--mono,monospace);line-height:1.6}.source{min-height:160px}.hint{color:var(--text-2);line-height:1.7}.error{color:var(--danger)}summary{cursor:pointer;min-height:32px;align-content:center}@media(max-width:600px){.toolbar label{flex-basis:100%}}
</style>
