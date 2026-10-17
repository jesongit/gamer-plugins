<template>
  <section class="video-workbench" data-testid="video-workbench">
    <nav class="workbench-tabs" aria-label="视频子页签">
      <button v-for="tab in tabs" :key="tab.key" class="tab-btn" :class="{ active: activeTab === tab.key }"
        type="button" :aria-pressed="activeTab === tab.key" :data-testid="`workbench-tab-${tab.key}`" @click="activeTab = tab.key">{{ tab.label }}</button>
    </nav>
    <MediaLibrary v-show="activeTab === 'library'" :media-list="mediaList" :loading="loading" :selected-id="selectedId" :load-error="mediaLoadError"
      @select="selectVideo" @refresh="refresh" @changed="refresh" @recording-finished="recordingFinished" @recording-selected="openSample" @imported="imported" />
    <VideoSamples v-if="sampleVisited" v-show="activeTab === 'sample'" :active="activeTab === 'sample'" :recordings="recordings"
      :recordings-error="recordingsError" :recording-id="sampleRecordingId" @refresh-recordings="loadRecordings" @open-library="activeTab = 'library'" @open-automation="openAutomation" />
  </section>
</template>
<script setup>
import { inject, onMounted, onBeforeUnmount, ref, watch } from 'vue'
import { WORKSPACE_CONTEXT_KEY } from '../../../../../../web/src/workspace/context'
import { api } from '../../../../../../web/src/api'
import MediaLibrary from './MediaLibrary.vue'
import VideoSamples from './VideoSamples.vue'
import { requestStageMedia } from '../../../../../../web/src/components/console/useConsoleStage'
import { videoApi } from './videoApi'
const tabs = [{ key: 'library', label: '视频' }, { key: 'sample', label: '素材包' }]
const workspace = inject(WORKSPACE_CONTEXT_KEY, null)
const toast = (message, type) => workspace?.uiBridge.toast.show({ message, type })
async function openAutomation() {
  try {
    const snapshot = await api.listExtensions()
    const enabled = (Array.isArray(snapshot) ? snapshot : snapshot.extensions || []).some(item => item.id === 'gamer-yaml' && item.state === 'running')
    if (!enabled) toast('请先在插件页启用自动化插件，再使用素材。', 'info')
    await workspace?.uiBridge.workspace.openPanel(enabled ? 'gamer-yaml:automation' : 'plugins')
  } catch (e) { toast('打开自动化失败：' + e.message, 'error') }
}
const activeTab = ref('library'), sampleVisited = ref(false)
const mediaList = ref([]), selectedId = ref(''), loading = ref(false), mediaLoadError = ref('')
const recordings = ref([]), recordingsError = ref(''), sampleRecordingId = ref('')
let mediaSeq = 0, recordingSeq = 0
watch(activeTab, tab => { if (tab === 'sample') { sampleVisited.value = true; void loadRecordings() } })
async function refresh() {
  const seq = ++mediaSeq; loading.value = true
  try {
    const list = await videoApi.listMedia()
    if (seq !== mediaSeq) return
    mediaList.value = list; mediaLoadError.value = ''
    if (!list.some(item => item.id === selectedId.value)) selectedId.value = ''
  } catch (e) { if (seq === mediaSeq) mediaLoadError.value = e.message || String(e) }
  finally { if (seq === mediaSeq) loading.value = false }
}
async function loadRecordings() {
  const seq = ++recordingSeq
  try { const rows = await videoApi.recordingHistory(); if (seq === recordingSeq) { recordings.value = rows; recordingsError.value = '' } }
  catch (e) { if (seq === recordingSeq) recordingsError.value = e.message || String(e) }
}
function selectVideo(id) { selectedId.value = id; requestStageMedia(id) }
async function imported(id) { await refresh(); if (mediaList.value.some(item => item.id === id)) selectVideo(id) }
async function recordingFinished(record) { await refresh(); const id = record?.segments?.find(segment => mediaList.value.some(item => item.id === segment.media_id))?.media_id; if (id) selectVideo(id); await loadRecordings() }
function openSample(record) { if (!record?.id) return; sampleRecordingId.value = record.id; activeTab.value = 'sample' }
onMounted(() => { void refresh(); void loadRecordings() })
onBeforeUnmount(() => { mediaSeq++; recordingSeq++ })
</script>
<style scoped>
.video-workbench{display:flex;flex-direction:column;gap:8px;min-height:0;min-width:0;flex:1;padding:var(--workspace-inset,8px);box-sizing:border-box;overflow:hidden}
.workbench-tabs{display:flex;gap:4px;border-bottom:1px solid var(--border);padding-bottom:6px;flex-shrink:0}
.tab-btn{height:28px;padding:3px 10px;border:1px solid transparent;border-radius:3px;background:transparent;color:var(--text-2);font-size:13px;cursor:pointer}
.tab-btn:hover{color:var(--text-0)}.tab-btn.active{border-color:var(--border);background:var(--bg-2);color:var(--text-0);font-weight:700}
.tab-btn:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
</style>
