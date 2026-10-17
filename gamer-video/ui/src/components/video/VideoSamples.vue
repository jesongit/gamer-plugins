<template>
  <section class="video-samples" data-testid="video-samples">
    <div class="sample-toolbar">
      <strong>演示素材包</strong>
      <button class="btn btn-sm" :disabled="busy" @click="loadSamples">刷新</button>
      <label class="btn btn-sm" :class="{ disabled: busy }">导入素材包<input type="file" accept=".gamersample,.zip" :disabled="busy" @change="importFile" /></label>
    </div>
    <p class="hint">选择录制范围，提取操作前后的截图和原始事件。素材包可交给自己的 Agent 编写脚本；流程是否完成由实际运行确认。</p>
    <div v-if="error" role="alert" class="error" data-testid="sample-error">{{ error }}</div>
    <div v-if="recordingsError" role="alert" class="error">{{ recordingsError }}</div>
    <label class="field">录制会话
      <select v-model="selectedRecording" data-testid="sample-recording" class="select" :disabled="busy" @change="chooseRecording">
        <option value="">请选择已结束的录制</option>
        <option v-for="record in endedRecordings" :key="record.id" :value="record.id">{{ recordingTime(record.started_at) }} · {{ record.event_count }} 个操作 · {{ recordingLabel(record.state) }}</option>
      </select>
    </label>
    <button v-if="!endedRecordings.length" class="btn btn-sm" @click="$emit('open-library')">前往素材库录制</button>
    <template v-if="session">
      <label class="field">素材名称<input v-model="name" data-testid="sample-name" class="input" maxlength="120" :disabled="busy" /></label>
      <div class="range-fields">
        <label class="field">开始时间（录制秒）<input v-model.number="startSeconds" class="input" type="number" min="0" step="0.001" aria-label="素材开始时间" :disabled="busy" @change="markChanged" /></label>
        <label class="field">结束时间（录制秒）<input v-model.number="endSeconds" class="input" type="number" min="0" step="0.001" aria-label="素材结束时间" :disabled="busy" @change="markChanged" /></label>
      </div>
      <p class="hint" v-if="recordedRange">可用范围：{{ seconds(recordedRange.start) }}–{{ seconds(recordedRange.end) }} 秒。截取本次要演示的完整操作。</p>
      <div class="sample-toolbar"><button class="btn btn-sm" :disabled="busy || !validRange" @click="previewBoundaries">查看开始 / 结束画面</button><span class="hint">当前范围 {{ rangeEvents.length }} 个操作 · 整段 {{ events.length }} 个</span></div>
      <div v-if="preview" class="boundary-preview" data-testid="sample-boundaries">
        <figure><img :src="preview.start.url" alt="开始时的原始画面" @load="preview.start.loaded = true" @error="preview.start.loaded = false" /><figcaption>开始 · {{ seconds(preview.start.timeline) }}s</figcaption></figure>
        <figure><img :src="preview.end.url" alt="结束时的原始画面" @load="preview.end.loaded = true" @error="preview.end.loaded = false" /><figcaption>结束 · {{ seconds(preview.end.timeline) }}s</figcaption></figure>
      </div>
      <details v-if="events.length"><summary>标注误操作或无效重复输入</summary>
        <p class="hint">记录会完整保留；勾选并说明误操作，便于阅读素材的人或外部 Agent 理解过程。</p>
        <div v-for="event in rangeEvents" :key="event.event_id">
          <label><input v-model="ignoredEventIds" type="checkbox" :value="event.event_id" :disabled="busy" />{{ seconds(event.timeline_us) }}s · {{ event.kind }} · {{ event.payload?.code || `${event.payload?.x ?? ''},${event.payload?.y ?? ''}` }}</label>
          <input v-if="ignoredEventIds.includes(event.event_id)" v-model="ignoreReasons[event.event_id]" class="input" placeholder="明确说明忽略原因，例如 Alt 误操作" :disabled="busy" />
        </div>
      </details>
      <button class="btn btn-primary" data-testid="sample-create" :disabled="!canCreate" @click="createSample">{{ busy ? '处理中…' : '制作素材包' }}</button>
      <p v-if="creationIssue && !busy" class="hint" role="status">{{ creationIssue }}</p>
    </template>
    <div v-if="result" class="sample-result" data-testid="sample-result">
      <strong>{{ result.name }}</strong> · {{ result.status === 'complete' ? '截图与事件已保存' : '素材不完整，请补充'  }}
      <ul v-if="result.diagnostics?.length"><li v-for="(d, i) in result.diagnostics" :key="i">{{ d.code }}：{{ d.message }}</li></ul>
      <template v-if="result.status === 'complete'"><p class="hint">可在工作稿中关联素材，或通过 MCP 交给自己的 Agent 读取。</p><button class="btn btn-sm" @click="emit('open-automation')">去自动化使用素材</button></template>
    </div>
    <ul class="sample-list">
      <li v-for="sample in samples" :key="sample.id" data-testid="sample-item">
        <div><strong>{{ sample.name || sample.path }}</strong><div class="hint">{{ sample.status === 'complete' ? '截图与事件' : sample.status === 'corrupt' ? '素材损坏' : '素材不完整' }}</div></div>
        <div class="sample-toolbar"><button class="btn btn-sm" :disabled="busy" @click="exportSample(sample.id)">导出</button><button class="btn btn-sm btn-danger" :disabled="busy" @click="deleteSample(sample)">删除</button></div>
      </li>
    </ul>
    <p v-if="!samples.length" class="hint">还没有素材包；选择一段已结束的录制制作，或导入已有素材包。</p>
  </section>
</template>

<script setup>
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { videoApi } from './videoApi'
import { useConfirmDialog } from '../../../../../../web/src/components/ui/useConfirmDialog'
import { inspectSampleArchive } from './sampleBundle'

const props = defineProps({ active: Boolean, recordings: { type: Array, default: () => [] }, recordingsError: String, recordingId: String })
const confirm = useConfirmDialog()
const emit = defineEmits(['update:recordingId', 'refresh-recordings', 'open-library', 'open-automation'])
const selectedRecording = ref(''), session = ref(null), events = ref([]), samples = ref([])
const name = ref('演示素材'), startSeconds = ref(0), endSeconds = ref(0)
const busy = ref(false), error = ref(''), preview = ref(null), result = ref(null)
const eventsReady = ref(false)
const ignoredEventIds = ref([]), ignoreReasons = ref({})
const rangeEvents = computed(() => events.value.filter(event => event.timeline_us >= startSeconds.value * 1e6 && event.timeline_us <= endSeconds.value * 1e6))
let generation = 0, listGeneration = 0
const endedRecordings = computed(() => props.recordings.filter(r => !['recording', 'finalizing'].includes(r.state)))
const recordedRange = computed(() => {
  const segments = session.value?.segments || []
  if (!segments.length) return null
  return { start: Math.min(...segments.map(segment => segment.start_us)), end: Math.max(...segments.map(segment => segment.start_us + segment.duration_us)) }
})
const validRange = computed(() => Number.isFinite(startSeconds.value) && Number.isFinite(endSeconds.value) && startSeconds.value >= 0 && endSeconds.value > startSeconds.value && !!recordedRange.value && Math.round(startSeconds.value * 1e6) >= recordedRange.value.start && Math.round(endSeconds.value * 1e6) <= recordedRange.value.end)
const creationIssue = computed(() => {
  if (!session.value) return '先选择一段已结束的录制。'
  if (!validRange.value) return '请在可用录制范围内填写开始和结束时间，结束时间须晚于开始时间。'
  if (!eventsReady.value) return '操作记录尚未读取成功，请重新选择录制后重试。'
  if (!name.value.trim()) return '请填写素材名称。'
  if (ignoredEventIds.value.some(id => rangeEvents.value.some(event => event.event_id === id) && !(ignoreReasons.value[id] || '').trim())) return '请说明每个忽略动作的原因，原始记录会完整保留。'
  return ''
})
const canCreate = computed(() => !busy.value && !creationIssue.value)
const recordingLabel = state => ({ completed:'已保存', cancelled:'已取消', failed:'保存异常' })[state] || state
const recordingTime = value => Number.isFinite(Date.parse(value)) ? new Date(value).toLocaleString() : value
const seconds = t => (t / 1e6).toFixed(3)
function markChanged() { preview.value = null; result.value = null; generation++ }
function reset() { generation++; session.value = null; events.value = []; eventsReady.value = false; preview.value = null; result.value = null; busy.value = false; error.value = '' }
watch(() => props.recordingId, async id => { if (id && id !== selectedRecording.value) { selectedRecording.value = id; await chooseRecording() } }, { immediate: true })
void loadSamples()
watch(() => props.active, active => { if (active) { emit('refresh-recordings'); void loadSamples() } })
onBeforeUnmount(() => { generation++; listGeneration++ })
async function loadSamples() {
  const seq = ++listGeneration
  try { const list = await videoApi.listSamples(); if (seq === listGeneration) samples.value = list }
  catch (e) { if (seq === listGeneration) error.value = `载入素材失败：${e.message || e}` }
}
async function chooseRecording() {
  reset()
  const id = selectedRecording.value, seq = generation
  if (!id) return
  busy.value = true
  try {
    const metadata = await videoApi.recordingStatus(id)
    if (seq !== generation) return
    if (['recording', 'finalizing'].includes(metadata.state)) throw new Error('请先结束录制并等待保存完成，再选择素材范围')
    session.value = metadata
    const segments = metadata.segments || []
    startSeconds.value = (segments[0]?.start_us || 0) / 1e6
    const last = segments.at(-1)
    endSeconds.value = last ? (last.start_us + last.duration_us) / 1e6 : 0
    try { const loaded = await videoApi.recordingEvents(id); if (seq === generation) { events.value = loaded; eventsReady.value = true } } catch (e) { if (seq === generation) error.value = `操作证据不可用：${e.message || e}` }
    if (seq === generation) emit('update:recordingId', id)
  } catch (e) { if (seq === generation) error.value = e.message || String(e) }
  finally { if (seq === generation) busy.value = false }
}
async function boundary(timeline, before) {
  const segment = (session.value?.segments || []).find(s => timeline >= s.start_us && timeline <= s.start_us + s.duration_us)
  if (!segment) throw new Error('标记位于没有视频的时间间隙')
  const pts = timeline - segment.start_us
  const info = await videoApi.mediaFrames(segment.media_id, { ptsUs: pts })
  let position = info.current
  if (before && position?.pts_us > pts) position = (await videoApi.mediaFrameNeighbors(segment.media_id, position.index)).prev
  if (before && !info.current && info.frame_count) position = await videoApi.mediaFrameNeighbors(segment.media_id, info.frame_count - 1)
  if (!position || !Number.isSafeInteger(position.index) || !Number.isSafeInteger(position.pts_us)) throw new Error('标记附近没有真实帧，请缩小范围')
  return { url: videoApi.mediaFrameUrl(segment.media_id, { index: position.index }), timeline: segment.start_us + position.pts_us }
}
async function previewBoundaries() {
  if (busy.value || !validRange.value) return
  const seq = ++generation
  busy.value = true; preview.value = null; error.value = ''
  try {
    const [start, end] = await Promise.all([boundary(Math.round(startSeconds.value * 1e6), false), boundary(Math.round(endSeconds.value * 1e6), true)])
    if (seq !== generation) return
    if (start.timeline >= end.timeline) throw new Error('START 与 END 必须是按顺序排列的不同画面')
    // Pin user confirmation to the exact frame identities returned by the server.
    startSeconds.value = start.timeline / 1e6; endSeconds.value = end.timeline / 1e6
    preview.value = { start, end }
  } catch (e) { if (seq === generation) error.value = e.message || String(e) }
  finally { if (seq === generation) busy.value = false }
}
async function createSample() {
  if (!canCreate.value) return
  const ignored = ignoredEventIds.value.filter(id => rangeEvents.value.some(event => event.event_id === id)).map(event_id => ({ event_id, reason: (ignoreReasons.value[event_id] || '').trim() }))
  if (ignored.some(item => !item.reason)) { error.value = '请为每个忽略动作填写原因'; return }
  const seq = ++generation
  busy.value = true; error.value = ''
  try {
    const response = await videoApi.createSample({ recording_id: selectedRecording.value, name: name.value.trim(), start_us: Math.round(startSeconds.value * 1e6), end_us: Math.round(endSeconds.value * 1e6), ...(ignored.length ? { ignored_actions: ignored } : {}) })
    if (seq !== generation) return
    result.value = response.manifest
    await loadSamples()
  } catch (e) { if (seq === generation) error.value = e.message || String(e) }
  finally { if (seq === generation) busy.value = false }
}
async function importFile(event) {
  const file = event.target.files?.[0]; event.target.value = ''
  if (!file || busy.value) return
  const seq = ++generation
  busy.value = true; error.value = ''
  try {
    const bundle = await inspectSampleArchive(file)
    if (seq !== generation) return
    await videoApi.importSample(bundle.bytes)
    if (seq !== generation) return
    result.value = bundle.manifest; await loadSamples()
  } catch (e) { if (seq === generation) error.value = `导入失败：${e.message || e}` }
  finally { if (seq === generation) busy.value = false }
}
async function exportSample(id) {
  busy.value = true; error.value = ''
  try { await videoApi.exportSample(id) } catch (e) { error.value = e.message || String(e) } finally { busy.value = false }
}
async function deleteSample(sample) {
  if (!await confirm(`删除素材包「${sample.name || sample.id}」？原视频会保留。`, { title: '删除素材包', confirmText: '删除', danger: true })) return
  busy.value = true; error.value = ''
  try { await videoApi.deleteSample(sample.id); await loadSamples() } catch (e) { error.value = e.message || String(e) } finally { busy.value = false }
}
</script>

<style scoped>
.video-samples{flex:1;min-height:0;overflow:auto}
.video-samples{display:flex;flex-direction:column;gap:10px;padding:10px;min-width:0}.sample-toolbar{display:flex;align-items:center;gap:8px;flex-wrap:wrap}.sample-toolbar input[type=file]{display:none}.field{display:flex;flex-direction:column;gap:5px;font-size:13px}.field .input,.field .select{width:100%;box-sizing:border-box}.range-fields,.boundary-preview{display:grid;grid-template-columns:1fr 1fr;gap:10px}.hint{color:var(--text-2);font-size:12px;line-height:1.6;margin:0}.check{display:flex;gap:6px;font-size:12px;align-items:flex-start}.error{color:var(--danger);font-size:13px;word-break:break-word}.boundary-preview figure{margin:0;min-width:0}.boundary-preview img{width:100%;max-height:240px;object-fit:contain;background:var(--bg-0)}figcaption{font-size:12px;color:var(--text-2)}.sample-result{padding:10px;border:1px solid var(--border);border-radius:6px;font-size:13px;overflow-wrap:anywhere}.sample-result ul{padding-left:18px;max-height:140px;overflow:auto}.sample-list{padding:0;list-style:none;margin:0}.sample-list li{display:flex;align-items:center;justify-content:space-between;gap:8px;padding:10px 0;border-bottom:1px solid var(--border);font-size:13px}.disabled{opacity:.5;pointer-events:none}
.video-samples{padding:2px;box-sizing:border-box}
</style>
