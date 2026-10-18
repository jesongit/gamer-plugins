<template>
  <section class="run-details-view" aria-label="运行详情">
    <div class="details-toolbar">
      <button class="btn" @click="$emit('edit')">{{ backLabel }}</button>
      <button v-if="canStop" class="btn btn-danger" :disabled="stopping || journal.record.state === 'stopping'" @click="stopRun">{{ stopping || journal.record.state === 'stopping' ? '正在停止…' : '停止运行' }}</button>
      <select v-model="journal.selected" class="select" aria-label="选择运行记录"><option value="" disabled>暂无运行记录</option><option v-for="r in journal.records" :key="r.run_id" :value="r.run_id">{{ new Date(r.started_at).toLocaleString() }} · {{ r.entrypoint }} · {{ stateText(r.state) }}</option></select>
      <button v-if="journal.record" class="btn" :disabled="journal.loading || journal.hasMore" @click="copy">{{ copied ? '已复制' : '复制日志' }}</button><button class="btn" @click="journal.history(); journal.refresh()">刷新</button>
    </div>
    <div class="timeline-hint"><span>{{ taskId ? '仅显示当前任务的运行 · 最近一次在前' : '当前设备的全部运行 · 切换脚本或函数保留所选日志' }}</span><button v-if="journal.hasOlder" class="btn btn-sm" :disabled="journal.olderLoading" @click="journal.loadOlder()">{{ journal.olderLoading ? '正在加载…' : '加载更早运行记录' }}</button></div>
    <div v-if="journal.record" class="run-summary">
      <strong :class="journal.record.state">{{ stateText(journal.record.state) }}</strong><span class="run-target" :title="journal.record.entrypoint">{{ journal.record.entrypoint }}</span><span>{{ elapsed }} 秒</span>
      <span v-if="target && target !== journal.record.entrypoint" class="other-target">此记录来自其他脚本或函数</span>
      <div class="run-counts"><span>{{ steps.length }} 个执行动作</span><span>成功 {{ counts.success }}</span><span v-if="counts.running">运行中 {{ counts.running }}</span><span v-if="counts.failed" class="failed">失败 {{ counts.failed }}</span><span v-if="counts.cancelled">停止 {{ counts.cancelled }}</span></div>
    </div>
    <section v-if="journal.record?.state === 'failed'" class="run-recovery" aria-label="运行失败后的下一步"><strong>这次运行没有完成</strong><p>先查看异常动作和当时的判断画面，再修改脚本。需要 Agent 协助时，可复制这次运行的引用。</p><button class="btn btn-sm" :disabled="journal.loading || journal.hasMore || !hasFailedAction" @click="showFailureDetails">{{ journal.loading || journal.hasMore ? '正在读取异常动作…' : hasFailedAction ? '查看异常动作' : '暂无异常动作记录' }}</button></section>
    <div v-if="journal.record" class="trace-toolbar"><button class="btn btn-sm" :aria-expanded="showTrace" @click="showTrace = !showTrace">判断画面 ({{ trace.images.length }})</button><button class="btn btn-sm" @click="copyAgentReference">复制 Agent 引用</button><button class="btn btn-sm" :disabled="!canReview" title="查看这次失败运行当时的工作稿版本" @click="reviewFailedSource">查看失败版本</button><button class="btn btn-sm" :disabled="!journal.record?.finished_at || !trace.images.length || trace.status === 'retained' || retaining" @click="retainTrace">{{ trace.status === 'retained' ? '证据已保留' : '长期保留证据' }}</button><span v-if="trace.loading">正在读取判断截图…</span><span v-else-if="trace.status === 'expired'">判断截图已过期</span><span v-else-if="!trace.enabled && !trace.error">未开启判断截图；基本日志仍可查看</span><span v-if="trace.gaps.length">{{ trace.gaps.length }} 处截图证据缺口</span></div>
    <div v-if="journal.record" class="trace-toolbar"><a v-if="journal.record.finished_at" class="btn btn-sm" :href="api.runDiagnosticsUrl(journal.selected)" :download="api.runDiagnosticsFilename(journal.selected)">下载诊断包</a><button v-else class="btn btn-sm" disabled>运行结束后可下载诊断包</button><span>包含本次记录和判断画面；参数值与私密配置不导出。</span></div>
    <p v-if="journal.detailsStatus === 'expired'" class="timeline-hint">详细步骤已到期，运行结果仍保留。诊断包会注明已缺少的证据。</p>
    <RunTraceImages v-if="showTrace" class="run-trace" :run-id="journal.selected" :images="trace.images" :status="trace.status" :enabled="trace.enabled" :gaps="trace.gaps" :collection="trace.collection" />
    <p v-if="trace.error" class="failure-text">{{ trace.error }}</p>
    <button v-if="showTrace && trace.hasMore" class="btn btn-sm" :disabled="trace.loading" @click="trace.refresh">加载更多图像</button>
    <div v-if="journal.error" class="failure-text" role="alert">{{ journal.error }}</div>
    <div v-if="journal.record?.error" class="failure-text">{{ journal.record.error }}</div>
    <div class="timeline-toolbar">
      <input v-model="query" class="input log-search" type="search" aria-label="搜索运行日志" placeholder="搜索步骤、函数、模板或日志…" />
      <label class="issue-filter"><input v-model="issuesOnly" type="checkbox" /> 仅异常</label>
      <div class="view-switch" role="group" aria-label="日志显示范围"><button :class="{ active: !includeFlow }" :aria-pressed="!includeFlow" @click="includeFlow = false">执行动作</button><button :class="{ active: includeFlow }" :aria-pressed="includeFlow" @click="includeFlow = true">全部步骤</button></div>
    </div>
    <div class="timeline-hint"><span>{{ query.trim() ? '搜索覆盖全部步骤' : includeFlow ? '包含函数调用、循环等流程步骤' : '省略外层调用与重复的失败记录' }} · 点击记录查看详情</span><span>{{ visibleRows.length }} 条</span></div>
    <div ref="stream" class="details-stream" @scroll="onScroll">
      <table v-if="visibleRows.length" class="journal-table" aria-label="运行执行记录">
        <colgroup><col class="time-column" /><col /><col class="state-column" /><col class="duration-column" /></colgroup>
        <thead><tr><th scope="col">时间</th><th scope="col">动作 / 结果</th><th scope="col">状态</th><th scope="col" class="duration-heading">耗时</th></tr></thead>
        <tbody><RunJournalRow v-for="row in visibleRows" :key="`${journal.selected}:${row.node.id}`" :node="row.node" :now="clock" :selected="selectedEvent === row.node.id" @select="selectEvent" /></tbody>
      </table>
      <p v-if="!visibleRows.length" class="empty">{{ rows.length ? '没有符合筛选条件的记录。' : journal.loading ? '正在读取运行详情…' : journal.record?.state === 'starting' ? '正在准备设备与执行环境…' : journal.record ? '本次运行没有步骤记录。' : '运行脚本后在这里查看步骤、匹配结果和日志。' }}</p>
      <p v-if="journal.hasMore" class="empty">正在读取后续记录…</p>
    </div>
    <RunJournalInspector v-if="selectedRow" :key="journal.selected" :row="selectedRow" :now="clock" :run-id="journal.selected" :images="trace.images" :trace-status="trace.status" :trace-enabled="trace.enabled" :trace-gaps="trace.gaps" :trace-collection="trace.collection" :allow-locate="!taskId" @close="selectedEvent = null" @locate="$emit('locate', trace.snapshot ? { ...$event, source_snapshot: trace.snapshot } : $event)" />
    <div class="details-footer"><span :title="journal.selected">已加载 {{ journal.events.length }} 条事件{{ journal.hasMore ? ' · 继续加载中' : ' · 服务端保存' }}</span><button class="btn btn-sm" @click="scrollStart">查看开头</button><button class="btn btn-sm" @click="resumeFollow">{{ follow && !filtering ? '正在跟随最新记录' : '回到最新' }}</button></div>
  </section>
</template>
<script setup>
import { computed, inject, nextTick, onScopeDispose, reactive, ref, toRef, watch } from 'vue'
import { useRunJournal } from './useRunJournal'
import { actionRunRows, buildRunTree, filterRunRows, flattenRunTree, stateText } from './run-journal'
import { api } from '../../../../../../web/src/api'
import { WORKSPACE_CONTEXT_KEY } from '../../../../../../web/src/workspace/context'
import { requestRunSourceReview } from './automationContextBridge'
import { useRunTrace } from './useRunTrace'
import RunTraceImages from './RunTraceImages.vue'
import RunJournalRow from './RunJournalRow.vue'
import RunJournalInspector from './RunJournalInspector.vue'
const props = defineProps({ deviceId: String, target: String, liveRun: String, taskId: { type: String, default: '' }, backLabel: { type: String, default: '返回编辑' }, visible: { type: Boolean, default: true } })
defineEmits(['edit', 'locate'])
const journal = reactive(useRunJournal(toRef(props, 'deviceId'), toRef(props, 'liveRun'), toRef(props, 'visible'), toRef(props, 'taskId')))
const stopping = ref(false)
const canStop = computed(() => ['starting', 'running', 'paused', 'stopping'].includes(journal.record?.state))
async function stopRun() {
  const id = journal.selected
  if (!canStop.value || stopping.value) return
  stopping.value = true
  try { await api.cancelRun(id); if (journal.selected === id) await journal.refresh() }
  catch (e) { if (journal.selected === id) journal.error = e.message }
  finally { stopping.value = false }
}
const trace = reactive(useRunTrace(toRef(journal, 'selected'), toRef(props, 'visible')))
const retaining = ref(false)
async function retainTrace() {
  const id = journal.selected; retaining.value = true
  try { await api.retainRunTrace(id); if (id === journal.selected) await trace.refresh() } catch (e) { if (id === journal.selected) journal.error = e.message } finally { retaining.value = false }
}
const showTrace = ref(false), workspace = inject(WORKSPACE_CONTEXT_KEY, null)
const canReview = computed(() => journal.record?.state === 'failed' && ['gamer-yaml', 'gamer-yaml-version'].includes(journal.record.runner_id) && String(journal.record.entrypoint || '').includes('/') && workspace?.getSnapshot?.().currentPackageId === journal.record.content_package)
async function copyAgentReference() {
  const record = journal.record
  if (!record) return
  try {
    await navigator.clipboard.writeText(JSON.stringify({ run_id: record.run_id, device_id: record.device_id, package_id: record.content_package, runner_id: record.runner_id, entrypoint: record.entrypoint, tools: ['gamer.core.run_status', 'gamer.core.run_events', 'gamer.core.run_trace', 'gamer.core.run_image'] }, null, 2))
    copied.value = true
  } catch (e) { journal.error = `复制失败：${e.message}` }
}
async function reviewFailedSource() {
  const record = journal.record
  if (!canReview.value || !record?.content_package) return
  const pkg = record.content_package, entrypoint = String(record.entrypoint || '')
  if (!entrypoint.startsWith(`${pkg}/`)) { journal.error = '运行的源码上下文不一致，请查看冻结源码快照'; return }
  requestRunSourceReview(pkg, entrypoint.slice(pkg.length + 1), record.run_id)
  try { await workspace?.uiBridge.workspace.openPanel('gamer-yaml:automation') }
  catch (e) { journal.error = e.message }
}
const tree = computed(() => buildRunTree(journal.events, journal.hasMore ? null : journal.record))
const rows = computed(() => flattenRunTree(tree.value))
const hasFailedAction = computed(() => rows.value.some(row => row.node.kind === 'step' && row.node.state === 'failed'))
const steps = computed(() => actionRunRows(rows.value).filter(row => row.node.kind === 'step'))
const counts = computed(() => steps.value.reduce((result, { node }) => { result[node.state]++; return result }, { success: 0, failed: 0, running: 0, cancelled: 0 }))
const query = ref(''), issuesOnly = ref(false), includeFlow = ref(false), selectedEvent = ref(null)
const selectedRow = computed(() => rows.value.find(row => row.node.id === selectedEvent.value))
const filtering = computed(() => !!query.value.trim() || issuesOnly.value)
const visibleRows = computed(() => filterRunRows(rows.value, { query: query.value, issuesOnly: issuesOnly.value, includeFlow: includeFlow.value }))
const clock = ref(Date.now()), stream = ref(null), follow = ref(false), copied = ref(false)
const timer = setInterval(() => { clock.value = Date.now() }, 1000)
onScopeDispose(() => clearInterval(timer))
const elapsed = computed(() => (Math.max(0, (Date.parse(journal.record?.finished_at) || clock.value) - Date.parse(journal.record?.started_at || new Date())) / 1000).toFixed(1))
function selectEvent(id) { selectedEvent.value = id; follow.value = false }
function showFailureDetails() {
  query.value = ''; issuesOnly.value = true; follow.value = false
  const failure = visibleRows.value.findLast(row => row.node.kind === 'step' && row.node.state === 'failed')
  if (failure) {
    selectedEvent.value = failure.node.id
    nextTick(() => stream.value?.querySelector('.journal-row.selected')?.scrollIntoView?.({ block: 'nearest' }))
  }
}
function onScroll() { const el = stream.value; if (el) follow.value = el.scrollHeight - el.scrollTop - el.clientHeight < 48 }
function scrollBottom() { const id = journal.selected; nextTick(() => { if (stream.value && follow.value && id === journal.selected) stream.value.scrollTop = stream.value.scrollHeight }) }
function scrollStart() { follow.value = false; if (stream.value) stream.value.scrollTop = 0 }
function resumeFollow() { query.value = ''; issuesOnly.value = false; follow.value = true; scrollBottom() }
watch(() => journal.events.length, () => { if (follow.value && !filtering.value) scrollBottom() })
watch(() => journal.selected, () => {
  follow.value = !!props.liveRun && journal.selected === props.liveRun
  copied.value = false; query.value = ''; issuesOnly.value = false; selectedEvent.value = null
  if (!follow.value) nextTick(scrollStart)
}, { immediate: true })
async function copy() {
  try { await navigator.clipboard.writeText(JSON.stringify({ run: journal.record, events: journal.events }, null, 2)); copied.value = true }
  catch (e) { journal.error = `复制失败：${e.message}` }
}
</script>
<style scoped>
.run-recovery{display:flex;flex:none;flex-direction:column;gap:6px;padding:8px;border:1px solid var(--border);border-radius:var(--radius-sm);font-size:13px}.run-recovery strong{color:var(--danger)}.run-recovery p{color:var(--text-2);line-height:1.6;margin:0}.run-recovery .btn{align-self:flex-start}
.trace-toolbar{display:flex;gap:8px;align-items:center;flex-wrap:wrap;font-size:12px;color:var(--text-2)}.run-trace{max-height:45%;overflow:auto;padding:8px;border:1px solid var(--border)}.run-details-view{display:flex;flex-direction:column;flex:1;min-height:0;min-width:0;gap:8px;overflow:auto}.details-toolbar,.details-footer,.run-summary,.timeline-toolbar{display:flex;align-items:center;gap:8px;flex-wrap:wrap}.details-toolbar .select{flex:1;min-width:140px;max-width:100%}.details-stream{flex:1 0 200px;min-height:200px;overflow:auto;overflow-anchor:none;border:1px solid var(--border);border-radius:4px}.run-summary{font-size:13px;padding:10px;background:var(--bg-0);border-radius:6px}.run-target{flex:1;min-width:80px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.other-target{flex-basis:100%;font-size:12px;color:var(--text-2)}.run-counts{display:flex;gap:12px;flex-basis:100%;font-size:12px;color:var(--text-2);flex-wrap:wrap}.failed,.failure-text{color:var(--danger)}.success{color:var(--ok)}.running,.starting{color:var(--accent)}.failure-text{font-size:12px;white-space:pre-wrap;overflow-wrap:anywhere;max-height:100px;overflow:auto}.log-search{flex:1;min-width:160px;width:0}.issue-filter{display:flex;align-items:center;gap:5px;font-size:12px;white-space:nowrap;cursor:pointer}.issue-filter input{accent-color:var(--accent)}.timeline-hint,.details-footer{display:flex;justify-content:space-between;gap:8px;font-size:12px;color:var(--text-2)}.empty{color:var(--text-2);padding:16px;font-size:13px}
.journal-table{width:100%;table-layout:fixed;border-collapse:separate;border-spacing:0}.time-column{width:76px}.state-column{width:62px}.duration-column{width:58px}.journal-table th{position:sticky;top:0;z-index:1;text-align:left;background:var(--bg-2);border-bottom:1px solid var(--border);font-weight:500;font-size:12px;color:var(--text-2);padding:8px}.journal-table .duration-heading{text-align:right}.view-switch{display:flex;flex:none;border:1px solid var(--border);border-radius:4px;overflow:hidden}.view-switch button{font:inherit;font-size:12px;cursor:pointer;border:0;padding:5px 9px;background:var(--bg-0);color:var(--text-2)}.view-switch button.active{background:var(--bg-3);color:var(--accent)}.view-switch button:focus-visible{outline:2px solid var(--accent);outline-offset:-2px}.details-footer>span{margin-right:auto}.timeline-hint>span:last-child{flex:none}.timeline-hint>span:first-child{min-width:0}@container(max-width:420px){.time-column{width:68px}.state-column{width:54px}.duration-column{width:48px}}.run-details-view{container-type:inline-size}
</style>
