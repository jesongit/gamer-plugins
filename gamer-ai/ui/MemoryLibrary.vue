<script setup>
import { computed, onBeforeUnmount, reactive, ref, watch } from 'vue'
import { api } from '../../../web/src/api'
import { budgetValue, eventDetails, displayTime, tokenUsage, usageValue } from './ai-format'
import { lineDiff } from './conversation-format'
import BudgetFields from './BudgetFields.vue'
import MemoryEvidence from './MemoryEvidence.vue'
import {DEFAULT_LIMITS,validLimits} from './budget-format'
const props = defineProps({ packageId: { type: String, default: '' } })
const emit = defineEmits(['attach'])
const query = ref(''), items = ref([]), selected = ref(null), revisions = ref([]), older = ref(null), sources = ref(null)
const total = ref(0), page = ref(0), jobs = ref([]), index = ref(null), retrieval = ref(null), diagnostics = ref([])
const historyTotal=ref(0)
const busy = ref(''), error = ref(''), feedback = ref(''), files = ref([]), fileInput = ref(null)
const includeInactive = ref(false), compareRevision = ref('')
const importLimits=reactive({...DEFAULT_LIMITS}),resumeLimits=reactive({...DEFAULT_LIMITS}),editingJob=ref('')
const diff = computed(() => older.value && selected.value ? lineDiff(older.value.body, selected.value.body) : [])
let serial = 0, timer, disposed = false
const call = (action, values = {}, packageId = props.packageId) => api.callExtension('gamer-ai', action, { content_package: packageId, ...values })
const statusLabel = value => ({ active: '启用', disabled: '停用', deleted: '已删除', merged: '已合并', pending: '待处理', running: '处理中', paused: '已暂停', completed: '已完成', cancelled: '已取消', failed: '失败' })[value] || value
async function list(reset = false) {
  if (!props.packageId) return
  const request = ++serial
  if (reset) page.value = 0
  busy.value = '查询记忆'; error.value = ''
  try {
    const result = query.value.trim()
      ? await call('memory.search', { query: query.value.trim(), limit: 30, validation:'any', include_inactive: includeInactive.value, mode: 'hybrid' })
      : await call('memory.list', { offset: page.value * 30, limit: 30, validation:'any', ...(includeInactive.value ? {} : { status: 'active' }) })
    if (disposed || request !== serial) return
    items.value = result.items || []; total.value = result.total ?? items.value.length
    retrieval.value = result.retrieval || null; diagnostics.value = result.diagnostics || []
  } catch (e) { if (!disposed && request === serial) error.value = e.message || '记忆检索失败' }
  finally { if (!disposed && request === serial) busy.value = '' }
}
async function open(item) {
  const request = ++serial; busy.value = '读取全文'; error.value = ''; older.value = null; sources.value = null
  try {
    const result = await call('memory.get', { id: item.id })
    if (disposed || request !== serial) return
    selected.value = { ...result.memory, source_conflicts: result.source_conflicts ?? result.memory?.source_conflicts }
    const history = await call('memory.history', { id: item.id, offset: 0, limit: 20 })
    if (disposed || request !== serial) return
    revisions.value = history.items || []; historyTotal.value=history.total || revisions.value.length;compareRevision.value = ''
  } catch (e) { if (!disposed && request === serial) error.value = e.message || '无法读取记忆正文' }
  finally { if (!disposed && request === serial) busy.value = '' }
}
async function compare() {
  const id = selected.value?.id, revision = Number(compareRevision.value)
  if (!id || !revision) { older.value = null; return }
  const request = ++serial
  try { const value = await call('memory.get', { id, revision }); if (!disposed && request === serial) older.value = { ...value.memory, source_conflicts: value.source_conflicts ?? value.memory?.source_conflicts } }
  catch (e) { if (!disposed) error.value = e.message || '无法读取旧版本' }
}
async function moreHistory() {
  const id=selected.value?.id,request=serial
  if(!id) return
  try {const value=await call('memory.history',{id,offset:revisions.value.length,limit:20});if(!disposed && request===serial){revisions.value.push(...(value.items || []));historyTotal.value=value.total || revisions.value.length}}
  catch(e) {if(!disposed) error.value=e.message || '历史读取失败'}
}
async function source(sourceId, revision) {
  const packageId = props.packageId, request = serial
  try { const value = await call('memory.source.get', { id: sourceId, ...(revision ? { revision } : {}) }); if (!disposed && packageId === props.packageId && request === serial) sources.value = { ...value.source, current_revision: value.current_revision, current_deleted: value.current_deleted, changed_since_reference: value.changed_since_reference } }
  catch (e) { if (!disposed) error.value = e.message || '来源已不可用' }
}
async function refreshJobs() {
  if (!props.packageId || disposed) return
  const packageId = props.packageId
  try {
    const [result, status] = await Promise.all([call('memory.jobs', { limit: 30 }), call('memory.index')])
    if (!disposed && props.packageId === packageId) { jobs.value = result.items || []; index.value = status }
  } catch (e) { if (!disposed && props.packageId === packageId) error.value = e.message || '无法读取作业进度' }
}
async function poll() { await refreshJobs(); if (!disposed) timer = setTimeout(poll, 1800) }
async function selectFiles(event) {
  files.value = [...(event.target.files || [])].map(file => ({ file, operation_id: crypto.randomUUID(), status: 'selected' }))
  error.value = ''
  for (const { file } of files.value) {
    if (!/\.(md|txt)$/i.test(file.name) || file.size > 1024 * 1024) { error.value = '仅支持每个不超过 1 MiB 的 MD/TXT 文件。'; files.value = []; break }
  }
}
function readText(file) { return new Promise((resolve, reject) => { const reader = new FileReader(); reader.onload = () => resolve(String(reader.result)); reader.onerror = () => reject(new Error('读取文件失败')); reader.readAsText(file, 'UTF-8') }) }
async function importFiles() {
  if (!files.value.length || !props.packageId || !validLimits(importLimits)) return
  busy.value = '暂存攻略'; error.value = ''; feedback.value = ''
  const packageId = props.packageId
  try {
    for (const input of files.value) {
      if (input.status === 'accepted') continue
      const text = await readText(input.file)
      if (disposed || props.packageId !== packageId) return
      if (!text.trim() || new TextEncoder().encode(text).length > 1024 * 1024) throw new Error('攻略需要非空 UTF-8 文本，且不能超过 1 MiB。')
      const result = await call('memory.import', { operation_id: input.operation_id, filename: input.file.name, title: input.file.name.replace(/\.(md|txt)$/i, ''), text,limits:{...importLimits} },packageId)
      input.status = 'accepted'; input.job_id = result.job_id
    }
    if (!disposed) { feedback.value = '攻略已暂存，由 AI 合并。提交收据和处理进度显示在下方，现有记忆保留。'; files.value = []; if (fileInput.value) fileInput.value.value = '' }
    await refreshJobs()
  } catch (e) { if (!disposed) error.value = `${e.message || '暂存失败'}；已接收的文件不会重复提交，未完成部分可以重试。` }
  finally { if (!disposed) busy.value = '' }
}
async function jobAction(job, action) {
  if(action==='resume' && !validLimits(resumeLimits)) return
  busy.value = '更新作业'; error.value = ''
  try { await call(`memory.job.${action}`, { job_id: job.id, operation_id: crypto.randomUUID(),...(action==='resume'?{limits:{...resumeLimits}}:{}) }); editingJob.value='';await refreshJobs() }
  catch (e) { if (!disposed) error.value = e.message || '作业操作失败' }
  finally { if (!disposed) busy.value = '' }
}
async function rebuild() {
  busy.value = '重建索引'; error.value = ''
  const packageId = props.packageId
  try { const value = await call('memory.rebuild'); if(!disposed && packageId===props.packageId) {index.value=value;feedback.value = '索引状态已更新。配置向量服务时，补齐向量可能使用供应商额度；关键词检索可独立使用。'} }
  catch (e) { error.value = e.message || '索引重建失败' }
  finally { busy.value = '' }
}
function attach() { if (selected.value) emit('attach', { id: selected.value.id, revision: selected.value.revision, title: selected.value.title, content_package: props.packageId }) }
watch(() => props.packageId, async () => {
  ++serial; selected.value = null; revisions.value = []; older.value = null; items.value = []; jobs.value = []; index.value = null; files.value=[]; sources.value=null; error.value=''; feedback.value=''
  clearTimeout(timer); await list(true); await refreshJobs(); if (!disposed) timer = setTimeout(poll, 1800)
}, { immediate: true })
onBeforeUnmount(() => { disposed = true; ++serial; clearTimeout(timer) })
</script>

<template>
  <section class="memory-panel">
    <header><div><h3>记忆库</h3><small>配置包 {{ packageId || '未选择' }} · 只读浏览</small></div><button :disabled="!packageId || !!busy" @click="list(true)">刷新记忆</button></header>
    <p>在对话中说明新增、修改、停用、删除或恢复，AI 会提交修订并保留收据。这里可读全文、查看来源与差异、附到对话。</p>
    <p v-if="error" role="alert" class="error">{{ error }}</p><p v-if="feedback" role="status">{{ feedback }}</p><p v-if="busy" role="status">{{ busy }}…</p>
    <form class="search" @submit.prevent="list(true)"><input v-model="query" aria-label="搜索记忆" placeholder="关键词、别名或操作描述" /><button :disabled="!packageId || !!busy">检索</button><label><input v-model="includeInactive" type="checkbox" @change="list(true)" />含停用与删除记录</label></form>
    <p v-if="retrieval" class="retrieval">关键词 {{ retrieval.keyword ? '可用' : '不可用' }} · 语义 {{ retrieval.semantic ? '可用' : '未启用' }}<span v-if="retrieval.degraded_reason"> · {{ retrieval.degraded_reason }}</span><span v-if="retrieval.pending_vectors"> · {{ retrieval.pending_vectors }} 个向量待补齐</span></p>
    <ul v-if="diagnostics.length" class="errors"><li v-for="(item,i) in diagnostics" :key="i">{{ typeof item === 'string' ? item : item.detail || item.message || item.code }}</li></ul>
    <div class="library-layout"><ul class="memory-list"><li v-for="item in items" :key="item.id"><button @click="open(item)"><b>{{ item.title }}</b><small>{{ statusLabel(item.status) }} · r{{ item.revision }}</small><MemoryEvidence :memory="item" /><span>{{ item.summary || item.applicability }}</span></button></li><li v-if="!items.length && !busy">暂无匹配记忆，可换关键词或通过对话新增。</li></ul>
      <article v-if="selected" class="memory-full"><header><h4>{{ selected.title }}</h4><button @click="attach">附到对话</button></header>
        <p>{{ statusLabel(selected.status) }} · 修订 {{ selected.revision }} · {{ selected.game_version === 'unknown' ? '适用版本未知' : selected.game_version }} · {{ displayTime(selected.updated_at) }}</p><MemoryEvidence :memory="selected" show-canonical />
        <p v-if="selected.applicability">适用条件：{{ selected.applicability }}</p><p v-if="selected.tags?.length">标签：{{ selected.tags.map(tag => tag === 'session_receipts_pending' ? '游玩记录草稿' : tag).join('、') }}</p><p v-if="selected.protected_fields?.length">人工指定保护：{{ selected.protected_fields.join('、') }}</p>
        <pre class="memory-body" aria-label="记忆全文">{{ selected.body }}</pre><p v-if="selected.reason">修订原因：{{ selected.reason }}</p>
        <details><summary>来源与原文</summary><ul><li v-for="(item,i) in selected.sources || []" :key="i"><pre>{{ eventDetails(item) || String(item) }}</pre><button v-if="item.source_id" @click="source(item.source_id,item.source_revision ?? item.revision)">读取来源原文</button></li></ul><template v-if="sources"><p>来源 {{ sources.title || sources.id }} · 显示修订 r{{ sources.revision }}</p><p v-if="sources.changed_since_reference || sources.current_deleted" class="source-warning">这是引用的历史原文；来源当前修订 r{{ sources.current_revision }}{{ sources.current_deleted ? '，已删除' : '，后来已修改' }}。请在对话中指示 AI 复核。</p><pre aria-label="来源全文">{{ sources.text }}</pre></template></details>
        <details><summary>修订历史与差异</summary><label>比较旧版本<select v-model="compareRevision" aria-label="比较修订版本" @change="compare"><option value="">请选择</option><option v-for="item in revisions" :key="item.revision" :value="item.revision">修订 {{ item.revision }} · {{ item.reason || item.updated_at }}</option></select></label><button v-if="revisions.length<historyTotal" @click="moreHistory">更多修订记录</button><pre v-if="older" class="diff" aria-label="修订差异"><span v-for="(row,i) in diff" :key="i" :class="row.type">{{ row.type === 'added' ? '+' : row.type === 'removed' ? '−' : ' ' }} {{ row.text }}
</span></pre><p v-if="older">原内容版本：{{ older.version || '历史快照' }} → 当前修订 {{ selected.revision }}</p></details>
      </article>
    </div>
    <div v-if="!query.trim() && total > 30" class="pagination"><button :disabled="page === 0 || !!busy" @click="page--;list()">上一页</button><span>{{ page+1 }} / {{ Math.ceil(total/30) }}</span><button :disabled="(page+1)*30 >= total || !!busy" @click="page++;list()">下一页</button></div>
    <section class="imports">
      <h4>导入攻略</h4>
      <p>支持 MD/TXT，每个文件最多 1 MiB。先暂存原文，AI 查询本包记忆后决定新增、补充或合并；不会直接覆盖人工定义。</p>
      <details><summary>合并预算 · 五项 0 表示无限</summary><BudgetFields :model-value="importLimits" prefix="合并" @update:model-value="Object.assign(importLimits,$event)" /><p>这是后台合并作业预算，与聊天和游玩分别累计。预算不足时可以暂停后调整继续。</p><p v-if="!validLimits(importLimits)" class="error">请填写合法的非零上限或 0。</p></details>
      <input ref="fileInput" type="file" accept=".md,.txt,text/markdown,text/plain" multiple aria-label="选择攻略文件" :disabled="!!busy" @change="selectFiles" />
      <ul><li v-for="input in files" :key="input.operation_id">{{ input.file.name }} · {{ input.status === 'accepted' ? '已接收' : '待暂存' }}</li></ul>
      <button :disabled="!files.length || !packageId || !!busy || !validLimits(importLimits)" @click="importFiles">暂存并交给 AI 合并</button>
      <ul class="job-list"><li v-for="job in jobs" :key="job.id">
        <b>{{ job.title }}</b><span>{{ statusLabel(job.status) }} · {{ job.processed }} / {{ job.total }}</span>
        <progress :value="job.processed" :max="Math.max(1,job.total)" />
        <small>新增 {{ job.counts?.created || 0 }} · 修订 {{ job.counts?.updated || 0 }} · 合并 {{ job.counts?.merged || 0 }} · 保留 {{ job.counts?.retained || 0 }} · 跳过 {{ job.counts?.skipped || 0 }} · 失败 {{ job.counts?.failed || 0 }}</small>
        <p v-if="job.usage">模型 {{ usageValue(job.usage,['turns']) }} / {{ budgetValue(job.limits,'max_turns') }} 轮 · 工具 {{ usageValue(job.usage,['actions']) }} / {{ budgetValue(job.limits,'max_actions') }} 次 · 活动 {{ usageValue(job.usage,['active_seconds']) }} / {{ budgetValue(job.limits,'max_seconds') }} 秒 · Token {{ tokenUsage(job.usage) }} / {{ budgetValue(job.limits,'max_tokens') }}</p>
        <p v-else>模型用量尚未返回。</p><p v-if="job.error" class="error">{{ job.error }}</p>
        <div><button v-if="['pending','running'].includes(job.status)" :disabled="!!busy" @click="jobAction(job,'pause')">暂停合并</button><button v-if="['paused','failed'].includes(job.status)" :disabled="!!busy" @click="editingJob=job.id;Object.assign(resumeLimits,job.limits || DEFAULT_LIMITS)">继续合并</button><button v-if="['pending','running','paused','failed'].includes(job.status)" :disabled="!!busy" @click="jobAction(job,'cancel')">取消作业</button></div>
        <form v-if="editingJob===job.id" @submit.prevent="jobAction(job,'resume')"><p>修改上限或设为 0 后明确继续，累计用量不会清零。</p><BudgetFields :model-value="resumeLimits" prefix="恢复合并" @update:model-value="Object.assign(resumeLimits,$event)" /><button type="submit" :disabled="!!busy || !validLimits(resumeLimits)">按预算继续合并</button></form>
      </li></ul>
    </section>
    <section v-if="index" class="index-status"><h4>检索索引</h4><p>关键词 {{ index.keyword_ready ? '就绪' : '待修复' }} · 语义 {{ index.semantic_ready ? '就绪' : '未就绪' }} · {{ index.total_memories }} 条记忆 · {{ index.total_chunks }} 个片段 · {{ index.pending_vectors }} 个向量待补齐</p><button :disabled="!!busy" @click="rebuild">重建索引</button><small>正文是权威来源，索引为本机派生缓存。重建不会删除正文。</small></section>
  </section>
</template>

<style scoped>
.memory-panel{padding:14px;overflow:auto;min-height:0;display:grid;gap:12px;font-size:12px}header{display:flex;justify-content:space-between;gap:8px;align-items:center}h3,h4,p{margin:0}p,small{color:var(--text-2,#aab5b2);line-height:1.7}.search{display:flex;gap:8px;flex-wrap:wrap}.search input:not([type=checkbox]){flex:1}.search label{display:flex;align-items:center;gap:4px}.library-layout{display:grid;grid-template-columns:minmax(120px,1fr) minmax(160px,2fr);gap:12px}.memory-list,.job-list{padding:0;list-style:none;margin:0;display:grid;gap:8px;align-content:start}.memory-list button{width:100%;text-align:left;display:grid;gap:6px}.memory-full{min-width:0;border:1px solid var(--border,#41484a);padding:12px;border-radius:6px;display:grid;gap:10px}.memory-body,pre{white-space:pre-wrap;overflow-wrap:anywhere;font:inherit;line-height:1.8;background:var(--bg-1,#181b1c);padding:8px;max-height:400px;overflow:auto}input,select,button{font:inherit;color:inherit;background:var(--bg-1,#181b1c);border:1px solid var(--border,#41484a);border-radius:4px;padding:7px;min-width:0}button{cursor:pointer}button:disabled{opacity:.5;cursor:default}summary{cursor:pointer}label{display:grid;gap:6px}.imports,.index-status,.job-list li{display:grid;gap:9px;padding:12px;border:1px solid var(--border,#41484a);border-radius:6px}.pagination{display:flex;gap:12px;align-items:center}.error,.removed{color:var(--danger,#ef9292)}.added{color:#77cbb4}.diff span{display:block}.job-list div{display:flex;gap:6px;flex-wrap:wrap}progress{width:100%;accent-color:var(--accent,#e4c956)}:focus-visible{outline:2px solid var(--accent,#e4c956);outline-offset:2px}@media(max-width:520px){.library-layout{grid-template-columns:1fr}}
.source-warning{color:var(--accent,#e4c956)}
.memory-panel{grid-template-columns:minmax(0,1fr);overflow-wrap:anywhere}.memory-panel>*{min-width:0}.memory-list li,.memory-list button{min-width:0}.memory-list b{overflow-wrap:anywhere}.memory-panel header{flex-wrap:wrap}
</style>
