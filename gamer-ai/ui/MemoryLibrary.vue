<script setup>
import { computed, nextTick, onBeforeUnmount, reactive, ref, watch } from 'vue'
import { api } from '../../../web/src/api'
import { budgetValue, eventDetails, displayTime, tokenUsage, usageValue } from './ai-format'
import { lineDiff, mergeEvents } from './conversation-format'
import BudgetFields from './BudgetFields.vue'
import MemoryEvidence from './MemoryEvidence.vue'
import RequestContext from './RequestContext.vue'
import AgentMarkdown from './AgentMarkdown.vue'
import {DEFAULT_LIMITS,validLimits} from './budget-format'
const props = defineProps({ packageId: { type: String, default: '' } })
const emit = defineEmits(['attach', 'settings'])
const query = ref(''), items = ref([]), selected = ref(null), revisions = ref([]), older = ref(null), sources = ref(null)
const total = ref(0), page = ref(0), jobs = ref([]), index = ref(null), retrieval = ref(null), diagnostics = ref([])
const historyTotal=ref(0)
const promptJob=ref(null),jobPrompts=ref([]),promptNext=ref(null),promptTotal=ref(0),promptLoading=ref(false),promptError=ref('')
const busy = ref(''), error = ref(''), feedback = ref(''), files = ref([]), fileInput = ref(null)
const includeInactive = ref(false), compareRevision = ref('')
const importLimits=reactive({...DEFAULT_LIMITS}),resumeLimits=reactive({...DEFAULT_LIMITS}),editingJob=ref('')
const diff = computed(() => older.value && selected.value ? lineDiff(older.value.body, selected.value.body) : [])
const activeJobs = computed(() => jobs.value.filter(job => ['pending','running'].includes(job.status)).length)
const attentionJobs = computed(() => jobs.value.filter(job => ['paused','failed'].includes(job.status)).length)
const panel = ref(null)
let listScrollTop = 0
let serial = 0, promptSerial=0, timer, disposed = false
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
  listScrollTop = panel.value?.scrollTop || 0
  const request = ++serial; busy.value = '读取全文'; error.value = ''; older.value = null; sources.value = null
  try {
    const result = await call('memory.get', { id: item.id })
    if (disposed || request !== serial) return
    selected.value = { ...result.memory, source_conflicts: result.source_conflicts ?? result.memory?.source_conflicts }
    await nextTick(); if (disposed || request !== serial) return
    if (panel.value) panel.value.scrollTop = 0
    const history = await call('memory.history', { id: item.id, offset: 0, limit: 20 })
    if (disposed || request !== serial) return
    revisions.value = history.items || []; historyTotal.value=history.total || revisions.value.length;compareRevision.value = ''
  } catch (e) { if (!disposed && request === serial) error.value = e.message || '无法读取记忆正文' }
  finally { if (!disposed && request === serial) busy.value = '' }
}
async function backToList() {
  ++serial;selected.value=null;revisions.value=[];older.value=null;sources.value=null;compareRevision.value=''
  if (busy.value==='读取全文') busy.value=''
  await nextTick(); if (panel.value) panel.value.scrollTop=listScrollTop
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
async function readJobPrompts(job, more=false) {
  const request=++promptSerial,packageId=props.packageId
  if (!more) { promptJob.value=job;jobPrompts.value=[];promptNext.value=null;promptTotal.value=0 }
  promptLoading.value=true;promptError.value=''
  try {
    const value=await call('memory.job.prompts',{job_id:job.id,after_seq:more?promptNext.value:0,limit:80})
    if (disposed || request!==promptSerial || packageId!==props.packageId) return
    jobPrompts.value=mergeEvents(more?jobPrompts.value:[],value.events).map(event=>({...event,id:`job-prompt:${event.seq}`}))
    promptNext.value=value.next_after_seq ?? null;promptTotal.value=value.total ?? jobPrompts.value.length
  } catch(e) { if (!disposed && request===promptSerial) promptError.value=e.message || '无法读取作业模型请求上下文' }
  finally { if (!disposed && request===promptSerial) promptLoading.value=false }
}
function closeJobPrompts() { ++promptSerial;promptJob.value=null;jobPrompts.value=[];promptNext.value=null;promptTotal.value=0;promptLoading.value=false;promptError.value='' }
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
  closeJobPrompts()
  ++serial; selected.value = null; revisions.value = []; older.value = null; items.value = []; jobs.value = []; index.value = null; files.value=[]; sources.value=null; error.value=''; feedback.value=''
  clearTimeout(timer); await list(true); await refreshJobs(); if (!disposed) timer = setTimeout(poll, 1800)
}, { immediate: true })
onBeforeUnmount(() => { disposed = true; ++serial; ++promptSerial; clearTimeout(timer) })
</script>

<template>
  <section ref="panel" class="memory-panel">
    <header><div><h3>记忆库</h3><small>配置包 {{ packageId || '未选择' }} · 只读浏览</small></div><button v-if="!selected" :disabled="!packageId || !!busy" @click="list(true)">刷新记忆</button></header>
    <p v-if="!selected" class="library-hint">点击记忆阅读全文。需要新增或修改时，在对话中告诉 Agent。</p>
    <p v-if="error" role="alert" class="error">{{ error }}</p><p v-if="feedback" role="status">{{ feedback }}</p><p v-if="busy" role="status">{{ busy }}…</p>
    <form v-if="!selected" class="search" @submit.prevent="list(true)"><input v-model="query" aria-label="搜索记忆" placeholder="关键词、别名或操作描述" /><button :disabled="!packageId || !!busy">检索</button><label><input v-model="includeInactive" type="checkbox" @change="list(true)" />含停用与删除记录</label></form>
    <p v-if="!selected && retrieval" class="retrieval">关键词 {{ retrieval.keyword ? '可用' : '不可用' }} · 语义 {{ retrieval.semantic ? '可用' : '未启用' }}<span v-if="retrieval.degraded_reason"> · {{ retrieval.degraded_reason }}</span><span v-if="retrieval.pending_vectors"> · {{ retrieval.pending_vectors }} 个向量待补齐</span></p>
    <ul v-if="!selected && diagnostics.length" class="errors"><li v-for="(item,i) in diagnostics" :key="i">{{ typeof item === 'string' ? item : item.detail || item.message || item.code }}</li></ul>
    <div class="library-layout"><ul v-if="!selected" class="memory-list"><li v-for="item in items" :key="item.id"><button class="memory-card" :title="item.title" :disabled="!!busy" @click="open(item)"><b>{{ item.title }}</b><small class="memory-meta">{{ statusLabel(item.status) }} · r{{ item.revision }}<span v-if="item.tags?.includes('session_receipts_pending')"> · 待整理原始经历</span></small><MemoryEvidence :memory="item" compact /><span v-if="item.summary || item.applicability" class="memory-summary">{{ item.summary || item.applicability }}</span></button></li><li v-if="!items.length && !busy" class="empty-library">暂无匹配记忆，可换关键词或通过对话新增。</li></ul>
      <article v-else class="memory-full"><header class="reader-actions"><button @click="backToList">← 返回记忆列表</button><button @click="attach">附到对话</button></header><h4>{{ selected.title }}</h4>
        <p class="memory-meta">{{ statusLabel(selected.status) }} · 修订 {{ selected.revision }} · {{ !selected.game_version || selected.game_version === 'unknown' ? '适用版本未知' : selected.game_version }}<span v-if="selected.updated_at"> · {{ displayTime(selected.updated_at) }}</span></p><MemoryEvidence :memory="selected" show-canonical />
        <p v-if="selected.tags?.includes('session_receipts_pending')" class="raw-experience">待整理的原始经历：目前保留游玩记录，尚未整理为攻略，使用前请核实适用条件。</p>
        <dl v-if="selected.applicability || selected.tags?.length || selected.protected_fields?.length" class="memory-metadata"><template v-if="selected.applicability"><dt>适用条件</dt><dd>{{ selected.applicability }}</dd></template><template v-if="selected.tags?.length"><dt>标签</dt><dd>{{ selected.tags.map(tag => tag === 'session_receipts_pending' ? '待整理原始经历' : tag).join('、') }}</dd></template><template v-if="selected.protected_fields?.length"><dt>人工指定保护</dt><dd>{{ selected.protected_fields.join('、') }}</dd></template></dl>
        <div class="memory-body" aria-label="记忆全文"><AgentMarkdown :text="selected.body" /></div><p v-if="selected.reason" class="revision-reason">修订原因：{{ selected.reason }}</p>
        <details class="reader-section sources-section"><summary>来源与原文<span> · {{ selected.sources?.length || 0 }} 个</span></summary><ul class="source-list"><li v-for="(item,i) in selected.sources || []" :key="i"><b>{{ item.title || item.filename || item.source_id || item.url || item.kind || (typeof item === 'string' ? item : '记录来源') }}</b><small v-if="item.source_revision ?? item.revision">引用修订 r{{ item.source_revision ?? item.revision }}</small><button v-if="item.source_id" @click="source(item.source_id,item.source_revision ?? item.revision)">读取来源原文</button><details class="source-receipt"><summary>来源记录详情</summary><pre>{{ eventDetails(item) || String(item) }}</pre></details></li></ul><p v-if="!selected.sources?.length">此记忆没有登记来源。</p><section v-if="sources" class="source-full"><p>来源 {{ sources.title || sources.id }} · 显示修订 r{{ sources.revision }}</p><p v-if="sources.changed_since_reference || sources.current_deleted" class="source-warning">这是引用的历史原文；来源当前修订 r{{ sources.current_revision }}{{ sources.current_deleted ? '，已删除' : '，后来已修改' }}。请在对话中指示 AI 复核。</p><div aria-label="来源全文"><AgentMarkdown :text="sources.text" /></div></section></details>
        <details class="reader-section history-section"><summary>修订历史与差异</summary><label>比较旧版本<select v-model="compareRevision" aria-label="比较修订版本" @change="compare"><option value="">请选择</option><option v-for="item in revisions" :key="item.revision" :value="item.revision">修订 {{ item.revision }} · {{ item.reason || item.updated_at }}</option></select></label><button v-if="revisions.length<historyTotal" @click="moreHistory">更多修订记录</button><pre v-if="older" class="diff" aria-label="修订差异"><span v-for="(row,i) in diff" :key="i" :class="row.type">{{ row.type === 'added' ? '+' : row.type === 'removed' ? '−' : ' ' }} {{ row.text }}
</span></pre><p v-if="older">原内容版本：{{ older.version || '历史快照' }} → 当前修订 {{ selected.revision }}</p></details>
      </article>
    </div>
    <div v-if="!selected && !query.trim() && total > 30" class="pagination"><button :disabled="page === 0 || !!busy" @click="page--;list()">上一页</button><span>{{ page+1 }} / {{ Math.ceil(total/30) }}</span><button :disabled="(page+1)*30 >= total || !!busy" @click="page++;list()">下一页</button></div>
    <details class="imports"><summary>导入攻略与整理任务<small>{{ jobs.length }} 个任务<span v-if="activeJobs"> · {{ activeJobs }} 个处理中</span><span v-if="attentionJobs" class="source-warning"> · {{ attentionJobs }} 个暂停或失败</span></small></summary><div class="section-body">
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
        <div><button @click="readJobPrompts(job)">查看请求上下文</button><button v-if="['pending','running'].includes(job.status)" :disabled="!!busy" @click="jobAction(job,'pause')">暂停合并</button><button v-if="['paused','failed'].includes(job.status)" :disabled="!!busy" @click="editingJob=job.id;Object.assign(resumeLimits,job.limits || DEFAULT_LIMITS)">继续合并</button><button v-if="['pending','running','paused','failed'].includes(job.status)" :disabled="!!busy" @click="jobAction(job,'cancel')">取消作业</button></div>
        <form v-if="editingJob===job.id" @submit.prevent="jobAction(job,'resume')"><p>修改上限或设为 0 后明确继续，累计用量不会清零。</p><BudgetFields :model-value="resumeLimits" prefix="恢复合并" @update:model-value="Object.assign(resumeLimits,$event)" /><button type="submit" :disabled="!!busy || !validLimits(resumeLimits)">按预算继续合并</button></form>
      </li></ul>
      <section v-if="promptJob" class="job-prompts" aria-label="记忆整理请求上下文"><header><h4>{{ promptJob.title }} · 请求上下文</h4><button @click="closeJobPrompts">关闭</button></header><p>只读记录 {{ jobPrompts.length }} / {{ promptTotal }} 轮；读取不会调用模型或恢复作业。</p><p v-if="promptError" role="alert" class="error">{{ promptError }}</p><p v-if="!jobPrompts.length && !promptLoading && !promptError">此作业尚未记录模型请求上下文；旧请求无法补回。</p><RequestContext v-if="jobPrompts.length" :contexts="jobPrompts" expanded @edit="emit('settings','prompts')" /><div class="prompt-actions"><button :disabled="promptLoading" @click="readJobPrompts(promptJob)">刷新请求记录</button><button v-if="promptNext!==null" :disabled="promptLoading" @click="readJobPrompts(promptJob,true)">加载后续请求</button><span v-if="promptLoading" role="status">正在读取…</span></div></section>
    </div></details>
    <details v-if="index" class="index-status"><summary>检索索引<small>{{ index.total_memories }} 条记忆 · {{ index.total_chunks }} 个片段</small></summary><div class="section-body"><p>关键词 {{ index.keyword_ready ? '就绪' : '待修复' }} · 语义 {{ index.semantic_ready ? '就绪' : '未就绪' }} · {{ index.pending_vectors }} 个向量待补齐</p><button :disabled="!!busy" @click="rebuild">重建索引</button><small>正文是权威来源，索引为本机派生缓存。重建不会删除正文。</small></div></details>
  </section>
</template>

<style scoped>
.memory-panel{flex:1;min-width:0;min-height:0;padding:16px;overflow:auto;display:grid;grid-template-columns:minmax(0,1fr);gap:16px;align-content:start;font-size:12px;overflow-wrap:anywhere;scrollbar-gutter:stable}.memory-panel>*{min-width:0}
header{display:flex;justify-content:space-between;gap:8px;align-items:center;flex-wrap:wrap}h3,h4,p{margin:0}h3{font-size:15px}p,small{color:var(--text-2,#aab5b2);line-height:1.8}.library-hint,.retrieval{font-size:11px}
input,select,button{font:inherit;color:inherit;background:var(--bg-1,#181b1c);border:1px solid var(--border,#41484a);border-radius:6px;padding:8px;min-width:0;max-width:100%;box-sizing:border-box}button{cursor:pointer}button:disabled{opacity:.5;cursor:default}summary{cursor:pointer;line-height:1.8}label{display:grid;gap:7px}:focus-visible{outline:2px solid var(--accent,#e4c956);outline-offset:2px}
.search{display:grid;grid-template-columns:minmax(0,1fr) auto;gap:8px}.search label{grid-column:1/-1;display:flex;align-items:center;gap:5px;font-size:11px}.search input[type=checkbox]{margin:0}
.library-layout{min-width:0}.memory-list,.job-list,.source-list{padding:0;list-style:none;margin:0;display:grid;gap:10px;align-content:start}.memory-list li,.memory-list button{min-width:0}.memory-list button{width:100%;text-align:left;display:grid;gap:7px;padding:13px 14px;background:var(--bg-2,#24282a);border-color:transparent}.memory-list button:hover:not(:disabled){border-color:var(--border,#41484a)}
.memory-list b{font-size:14px;line-height:1.65;overflow-wrap:anywhere;display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:2;overflow:hidden}.memory-meta{font-size:11px;color:var(--text-2,#aab5b2)}.memory-summary{font-size:12px;line-height:1.8;color:var(--text-2,#aab5b2);display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:2;overflow:hidden}.empty-library{padding:20px 0;color:var(--text-2,#aab5b2);line-height:1.8}
.memory-full{min-width:0;display:grid;gap:14px}.memory-full h4{font-size:17px;line-height:1.6}.reader-actions{margin-bottom:2px}.reader-actions button:first-child{padding-left:0;border-color:transparent;background:transparent;font-size:11px}.memory-metadata{margin:0;padding:12px 14px;background:var(--bg-2,#24282a);border-radius:7px;display:grid;gap:4px;font-size:11px;line-height:1.8}.memory-metadata dt{font-weight:600;color:var(--text-2,#aab5b2)}.memory-metadata dd{margin:0 0 6px}.memory-metadata dd:last-child{margin-bottom:0}
.memory-body{min-width:0;padding:18px 0;border-top:1px solid var(--border,#41484a);border-bottom:1px solid var(--border,#41484a)}.raw-experience{padding:10px 12px;border-left:2px solid var(--accent,#e4c956);background:var(--bg-2,#24282a)}.revision-reason{font-size:11px}
.reader-section{min-width:0}.reader-section>summary{font-size:12px;font-weight:600;padding:7px 0}.reader-section>summary span{font-weight:400;color:var(--text-2,#aab5b2)}.reader-section[open]>summary{margin-bottom:10px}.source-list li,.source-full{min-width:0;display:grid;gap:8px;padding:12px;background:var(--bg-2,#24282a);border-radius:7px}.source-list b{font-size:12px;line-height:1.8}.source-list button{justify-self:start}.source-full{margin-top:12px}.source-receipt{font-size:11px;color:var(--text-2,#aab5b2)}.history-section label{margin-bottom:10px}
pre{white-space:pre-wrap;overflow-wrap:anywhere;font:11px/1.8 Consolas,monospace;background:var(--bg-1,#181b1c);padding:10px;border-radius:6px;max-width:100%;box-sizing:border-box}.diff,.source-receipt pre{max-height:320px;overflow:auto}.diff span{display:block}.error,.removed{color:var(--danger,#ef9292)}.added{color:#77cbb4}.source-warning{color:var(--accent,#e4c956)}
.imports,.index-status{min-width:0;border-top:1px solid var(--border,#41484a);padding-top:14px}.imports>summary,.index-status>summary{font-weight:600}.imports>summary small,.index-status>summary small{display:block;font-weight:400;font-size:11px;padding-left:16px}.section-body{min-width:0;display:grid;gap:12px;margin-top:14px}.job-list li{min-width:0;display:grid;gap:9px;padding:12px;border:1px solid var(--border,#41484a);border-radius:7px}.job-list div,.prompt-actions{display:flex;gap:6px;flex-wrap:wrap}.job-list form{display:grid;gap:10px}.pagination{display:flex;gap:12px;align-items:center;justify-content:center;font-size:11px}progress{width:100%;accent-color:var(--accent,#e4c956)}
</style>
