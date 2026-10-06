<template>
  <section class="video-samples" data-testid="video-samples">
    <div class="sample-toolbar">
      <strong>演示素材包</strong>
      <button class="btn btn-sm" :disabled="busy || !packageId" @click="loadSamples">刷新</button>
      <label class="btn btn-sm" :class="{ disabled: busy || !packageId }">导入素材包<input type="file" accept=".gamersample,.zip" :disabled="busy || !packageId" @change="importFile" /></label>
    </div>
    <p class="hint">选择真实的 START 与 END 画面，确认目标后制作可携带素材。停止录制不代表目标已经完成。脚本生成在自动化面板中进行。</p>
    <div v-if="error" role="alert" class="error" data-testid="sample-error">{{ error }}</div>
    <div v-if="recordingsError" role="alert" class="error">{{ recordingsError }}</div>
    <label class="field">录制会话
      <select v-model="selectedRecording" data-testid="sample-recording" class="select" :disabled="busy" @change="chooseRecording">
        <option value="">请选择已结束的录制</option>
        <option v-for="record in endedRecordings" :key="record.id" :value="record.id">{{ record.started_at }} · {{ record.event_count }} 个操作 · {{ record.state }}</option>
      </select>
    </label>
    <button v-if="!endedRecordings.length" class="btn btn-sm" @click="$emit('open-library')">前往素材库录制</button>
    <template v-if="session">
      <label class="field">素材名称<input v-model="name" data-testid="sample-name" class="input" maxlength="120" :disabled="busy" /></label>
      <div class="range-fields">
        <label class="field">START（会话秒）<input v-model.number="startSeconds" class="input" type="number" min="0" step="0.001" :disabled="busy" @change="markChanged" /></label>
        <label class="field">END（会话秒）<input v-model.number="endSeconds" class="input" type="number" min="0" step="0.001" :disabled="busy" @change="markChanged" /></label>
      </div>
      <div class="sample-toolbar"><button class="btn btn-sm" :disabled="busy || !validRange" @click="previewBoundaries">查看 START / END</button><span class="hint">{{ events.length }} 个已记录操作</span></div>
      <div v-if="preview" class="boundary-preview" data-testid="sample-boundaries">
        <figure><img :src="preview.start.url" alt="START 原始画面" @load="preview.start.loaded = true" @error="preview.start.loaded = false; confirmed = false" /><figcaption>START · {{ seconds(preview.start.timeline) }}s</figcaption></figure>
        <figure><img :src="preview.end.url" alt="END 原始画面" @load="preview.end.loaded = true" @error="preview.end.loaded = false; confirmed = false" /><figcaption>END · {{ seconds(preview.end.timeline) }}s</figcaption></figure>
      </div>
      <label class="field">完成目标与 END 画面依据<textarea v-model="goal" data-testid="sample-goal" class="input" rows="3" maxlength="2000" placeholder="例如：每日奖励已领取，END 画面中的按钮显示已领取" :disabled="busy" @input="confirmed = false" /></label>
      <label class="check"><input v-model="confirmed" type="checkbox" :disabled="busy || !preview?.start.loaded || !preview?.end.loaded" data-testid="sample-goal-confirmed" />我已检查 END 画面，确认上述目标已完成</label>
      <label class="check"><input v-model="includeClips" type="checkbox" :disabled="busy" />附带原始视频片段（推荐，用于验证等待过程；总量不超过 128 MiB）</label>
      <p class="hint">保留 START、END 与操作前后的原尺寸帧；等待过程从附带视频按需取帧。最长 5 分钟、128 MiB，超过请分段录制。取消附带视频时需要更密集的画面证据，建议每段不超过 45 秒。</p>
      <button class="btn btn-primary" data-testid="sample-create" :disabled="!canCreate" @click="createSample">{{ busy ? '处理中…' : '制作素材包' }}</button>
    </template>
    <div v-if="result" class="sample-result" data-testid="sample-result">
      <strong>{{ result.name }}</strong> · {{ result.status === 'complete' ? '证据完整，待自动化验证' : '无法验证，需要补充证据' }}
      <ul v-if="result.diagnostics?.length"><li v-for="(d, i) in result.diagnostics" :key="i">{{ d.code }}：{{ d.message }}</li></ul>
    </div>
    <ul class="sample-list">
      <li v-for="sample in samples" :key="sample.path" data-testid="sample-item">
        <div><strong>{{ sample.name || sample.path }}</strong><div class="hint">{{ sample.status === 'complete' ? '证据完整' : sample.status === 'corrupt' ? '素材损坏' : '无法验证' }}</div></div>
        <a v-if="sample.id" class="btn btn-sm" :href="videoApi.sampleUrl(packageId, sample.id)" :download="`${sample.id}.gamersample`">导出</a>
      </li>
    </ul>
    <p v-if="!samples.length" class="hint">当前配置包还没有演示素材</p>
  </section>
</template>

<script setup>
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { videoApi } from './videoApi'
import { inspectSampleArchive } from './sampleBundle'

const props = defineProps({ active: Boolean, recordings: { type: Array, default: () => [] }, recordingsError: String, recordingId: String, packageId: String })
const emit = defineEmits(['update:recordingId', 'refresh-recordings', 'open-library'])
const selectedRecording = ref(''), session = ref(null), events = ref([]), samples = ref([])
const name = ref('演示素材'), startSeconds = ref(0), endSeconds = ref(0), goal = ref(''), confirmed = ref(false), includeClips = ref(true)
const busy = ref(false), error = ref(''), preview = ref(null), result = ref(null)
let generation = 0, listGeneration = 0
const endedRecordings = computed(() => props.recordings.filter(r => !['recording', 'finalizing'].includes(r.state)))
const validRange = computed(() => Number.isFinite(startSeconds.value) && Number.isFinite(endSeconds.value) && startSeconds.value >= 0 && endSeconds.value > startSeconds.value && endSeconds.value - startSeconds.value <= 300)
const canCreate = computed(() => !busy.value && props.packageId && session.value && validRange.value && name.value.trim() && goal.value.trim() && confirmed.value && preview.value?.start.loaded && preview.value?.end.loaded)
const seconds = t => (t / 1e6).toFixed(3)
function markChanged() { preview.value = null; confirmed.value = false; result.value = null; generation++ }
function reset() { generation++; session.value = null; events.value = []; preview.value = null; confirmed.value = false; result.value = null; busy.value = false; error.value = '' }
watch(() => props.recordingId, async id => { if (id && id !== selectedRecording.value) { selectedRecording.value = id; await chooseRecording() } }, { immediate: true })
watch(() => props.packageId, () => { reset(); samples.value = []; if (props.packageId) void loadSamples(); if (selectedRecording.value) void chooseRecording() }, { immediate: true })
watch(() => props.active, active => { if (active) { emit('refresh-recordings'); void loadSamples() } })
onBeforeUnmount(() => { generation++; listGeneration++ })
async function loadSamples() {
  const seq = ++listGeneration, pkg = props.packageId
  if (!pkg) return
  try { const list = await videoApi.listSamples(pkg); if (seq === listGeneration && pkg === props.packageId) samples.value = list }
  catch (e) { if (seq === listGeneration && pkg === props.packageId) error.value = `载入素材失败：${e.message || e}` }
}
async function chooseRecording() {
  reset()
  const id = selectedRecording.value, seq = generation
  if (!id) return
  busy.value = true
  try {
    const metadata = await videoApi.recordingStatus(id)
    if (seq !== generation) return
    if (['recording', 'finalizing'].includes(metadata.state)) throw new Error('请先结束录制，再选择 START 与 END')
    session.value = metadata
    const segments = metadata.segments || []
    startSeconds.value = (segments[0]?.start_us || 0) / 1e6
    const last = segments.at(-1)
    endSeconds.value = last ? (last.start_us + last.duration_us) / 1e6 : 0
    try { const loaded = await videoApi.recordingEvents(id); if (seq === generation) events.value = loaded } catch (e) { if (seq === generation) error.value = `操作证据不可用：${e.message || e}` }
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
  busy.value = true; confirmed.value = false; preview.value = null; error.value = ''
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
  const seq = ++generation, pkg = props.packageId
  busy.value = true; error.value = ''
  try {
    const response = await videoApi.createSample({ package_id: pkg, recording_id: selectedRecording.value, name: name.value.trim(), start_us: Math.round(startSeconds.value * 1e6), end_us: Math.round(endSeconds.value * 1e6), goal: { description: goal.value.trim(), confirmed: true }, include_clips: includeClips.value })
    if (seq !== generation || pkg !== props.packageId) return
    result.value = response.manifest
    await loadSamples()
  } catch (e) { if (seq === generation) error.value = e.message || String(e) }
  finally { if (seq === generation) busy.value = false }
}
async function importFile(event) {
  const file = event.target.files?.[0]; event.target.value = ''
  if (!file || busy.value || !props.packageId) return
  const seq = ++generation, pkg = props.packageId
  busy.value = true; error.value = ''
  try {
    const bundle = await inspectSampleArchive(file)
    if (seq !== generation || pkg !== props.packageId) return
    await videoApi.importSample(pkg, bundle.manifest.id, bundle.bytes)
    if (seq !== generation || pkg !== props.packageId) return
    result.value = bundle.manifest; await loadSamples()
  } catch (e) { if (seq === generation) error.value = `导入失败：${e.message || e}` }
  finally { if (seq === generation) busy.value = false }
}
</script>

<style scoped>
.video-samples{display:flex;flex-direction:column;gap:10px;padding:10px;min-width:0}.sample-toolbar{display:flex;align-items:center;gap:8px;flex-wrap:wrap}.sample-toolbar input[type=file]{display:none}.field{display:flex;flex-direction:column;gap:5px;font-size:13px}.field .input,.field .select{width:100%;box-sizing:border-box}.range-fields,.boundary-preview{display:grid;grid-template-columns:1fr 1fr;gap:10px}.hint{color:var(--text-2);font-size:12px;line-height:1.6;margin:0}.check{display:flex;gap:6px;font-size:12px;align-items:flex-start}.error{color:var(--danger);font-size:13px;word-break:break-word}.boundary-preview figure{margin:0;min-width:0}.boundary-preview img{width:100%;max-height:240px;object-fit:contain;background:var(--bg-0)}figcaption{font-size:12px;color:var(--text-2)}.sample-result{padding:10px;border:1px solid var(--border);border-radius:6px;font-size:13px;overflow-wrap:anywhere}.sample-result ul{padding-left:18px;max-height:140px;overflow:auto}.sample-list{padding:0;list-style:none;margin:0}.sample-list li{display:flex;align-items:center;justify-content:space-between;gap:8px;padding:10px 0;border-bottom:1px solid var(--border);font-size:13px}.disabled{opacity:.5;pointer-events:none}
</style>
