<script setup>
import { computed, ref, watch } from 'vue'

const props = defineProps({
  status: { type: Object, default: () => ({ state: 'stopped' }) },
  queue: { type: Object, default: () => ({ waiting: [] }) },
  devices: { type: Array, default: () => [] },
  output: { type: Object, required: true },
  busy: Boolean,
  perform: { type: Function, required: true },
})
const emit = defineEmits(['configure-target', 'open-rules', 'open-logs'])
const videoFolder = ref(''), folderEdited = ref(false), listing = ref(null), operation = ref('')
const active = computed(() => !['stopped', 'failed'].includes(props.status.state || 'stopped'))
const queueStatus = computed(() => props.status.queue || props.queue)
const target = computed(() => queueStatus.value.target)
const targetDevice = computed(() => props.devices.find(device => device.id === target.value?.device_id))
const current = computed(() => queueStatus.value.current)
const waitingCount = computed(() => queueStatus.value.waiting?.length || 0)
const states = { stopped: '未启动', preparing: '准备直播中', video: '视频轮播中', executing: '互动执行中', paused: '等待处理', stopping: '正在停止', failed: '自动直播失败' }
const stateLabel = computed(() => states[props.status.state] || '未启动')
const currentVideo = computed(() => {
  const video = props.status.stream?.video_file
  return typeof video === 'string' ? video.split(/[\\/]/).pop() : video?.name || video?.file || ''
})
const currentPicture = computed(() => {
  if (props.status.stream?.source_kind === 'device') return '执行目标实时画面'
  if (props.status.stream?.source_kind === 'video_folder') return '文件夹视频'
  return { preparing: '正在准备输出', executing: '执行目标实时画面', paused: '等待恢复输出', stopping: '等待当前运行结束', failed: '输出失败' }[props.status.state] || '文件夹视频'
})
const videoPosition = computed(() => {
  const seconds = props.status.stream?.video_position_secs
  if (!Number.isFinite(seconds)) return ''
  const total = Math.max(0, Math.floor(seconds))
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, '0')}`
})
const canStart = computed(() => videoFolder.value.trim() && target.value?.device_id && target.value?.package_id && targetDevice.value?.capabilities?.media_output !== false)
const files = computed(() => listing.value?.files || [])
const fileName = file => typeof file === 'string' ? file.split(/[\\/]/).pop() : file.name || file.path?.split(/[\\/]/).pop() || ''
watch(() => props.status.video_folder, folder => {
  if (folder && (!folderEdited.value || active.value)) videoFolder.value = folder
}, { immediate: true })
function editFolder() { folderEdited.value = true; listing.value = null }
async function preview() {
  if (props.busy || active.value) return
  operation.value = 'preview'
  try {
    const result = await props.perform('automatic.preview', { video_folder: videoFolder.value.trim() })
    if (result && Array.isArray(result.files)) listing.value = result
  } finally { operation.value = '' }
}
async function start() {
  if (!canStart.value || props.busy || active.value) return
  operation.value = 'start'
  try {
    const result = await props.perform('automatic.start', {
      video_folder: videoFolder.value.trim(),
      device_id: target.value.device_id,
      package_id: target.value.package_id,
      mode: props.output.mode,
      push_url: props.output.mode === 'rtmp' ? props.output.push_url : '',
      audio: props.output.audio,
      fps: props.output.fps,
      bitrate_kbps: props.output.bitrate_kbps,
    })
    if (result) { props.output.push_url = ''; folderEdited.value = false }
  } finally { operation.value = '' }
}
async function stop() {
  if ((!props.busy || operation.value === 'start') && active.value && props.status.state !== 'stopping') {
    operation.value = 'stop'
    try { await props.perform('automatic.stop') } finally { operation.value = '' }
  }
}
</script>

<template>
  <div class="automatic-live">
    <p class="automatic-state" role="status">{{ stateLabel }}</p>
    <p class="hint">空闲时轮播视频；弹幕触发后切到执行目标的实时画面，依次执行互动。队列清空后，视频从原位置继续。</p>
    <form @submit.prevent="start">
      <fieldset :disabled="busy || active">
        <label>视频文件夹<input v-model="videoFolder" aria-label="视频文件夹" required placeholder="例如 D:\直播视频" @input="editFolder" /></label>
        <p class="hint">填写运行 Gamer 服务端的电脑上的文件夹路径，按文件名顺序循环播放。不读取子文件夹。</p>
        <button type="button" :disabled="!videoFolder.trim()" @click="preview">{{ operation === 'preview' ? '正在读取视频…' : '查看视频列表' }}</button>
        <div v-if="listing" class="folder-preview" role="status">
          <p v-if="files.length">找到 {{ files.length }} 个视频，按以下顺序循环：</p>
          <p v-else>此文件夹没有可播放的视频，请选择其他文件夹。</p>
          <ol v-if="files.length"><li v-for="(file, index) in files.slice(0, 50)" :key="index">{{ fileName(file) }}</li></ol>
          <p v-if="files.length > 50" class="hint">这里只显示前 50 个，播放时使用完整列表。</p>
        </div>
        <label>输出方式<select v-model="output.mode" aria-label="自动直播输出方式"><option value="local">直播姬多媒体素材（推荐）</option><option value="rtmp">手动 RTMP / RTMPS 推流</option></select></label>
        <label v-if="output.mode === 'rtmp'">完整推流地址<input v-model="output.push_url" type="password" aria-label="自动直播推流地址" autocomplete="off" required placeholder="rtmp://服务器/路径/推流码" /></label>
        <div class="row"><label>帧率<input v-model.number="output.fps" type="number" aria-label="自动直播帧率" min="10" max="60" required /></label><label>码率 Kbps<input v-model.number="output.bitrate_kbps" type="number" aria-label="自动直播码率" min="500" max="20000" required /></label></div>
        <label class="check"><input v-model="output.audio" type="checkbox" />包含视频和游戏声音</label>
        <p v-if="target?.device_id?.startsWith('browser-')" class="hint">浏览器目标的实时画面为静音；文件夹视频可包含原视频声音。</p>
        <div class="target-summary">
          <p v-if="target">互动执行目标：{{ targetDevice?.name || target.device_id }} · {{ target.package_id }}</p>
          <p v-else>先在下方保存互动执行目标，再启动自动直播。</p>
          <p v-if="targetDevice?.capabilities?.media_output === false" class="error">当前执行目标不支持音视频输出，请选择其他目标。</p>
          <button type="button" @click="emit('configure-target')">{{ target ? '查看执行目标' : '设置执行目标' }}</button>
        </div>
        <button type="submit" class="primary" :disabled="!canStart">{{ operation === 'start' ? '正在启动…' : '启动自动直播' }}</button>
      </fieldset>
      <button v-if="active" type="button" class="stop" :disabled="(busy && operation !== 'start') || status.state === 'stopping'" @click="stop">{{ status.state === 'stopping' || operation === 'stop' ? '正在停止…' : '停止自动直播' }}</button>
    </form>
    <div v-if="active || status.state === 'failed'" class="activity" aria-label="自动直播运行状态">
      <dl>
        <div><dt>当前画面</dt><dd>{{ currentPicture }}</dd></div>
        <div v-if="currentVideo"><dt>当前视频</dt><dd>{{ currentVideo }}</dd></div>
        <div v-if="currentVideo && videoPosition"><dt>视频位置</dt><dd>{{ videoPosition }}</dd></div>
        <div v-if="current"><dt>当前互动</dt><dd>{{ current.name || current.entrypoint }}<span v-if="current.state === 'cancelling'"> · 正在取消</span></dd></div>
        <div><dt>等待互动</dt><dd>{{ waitingCount }} 项</dd></div>
      </dl>
      <p v-if="status.state === 'paused'" class="hint">当前问题解决后，可在「触发日志」中恢复执行。等待项会保留。</p>
      <p v-if="status.state === 'stopping'" class="hint">正在停止受理新触发、清理等待队列和当前运行，然后关闭输出。</p>
    </div>
    <p v-if="status.error" class="error" role="alert">{{ status.error }}</p>
    <div class="actions"><button type="button" @click="emit('open-rules')">配置弹幕规则</button><button type="button" @click="emit('open-logs')">查看触发日志</button></div>
    <p class="hint">停止自动直播会停止当前互动、清空等待项并关闭输出；互动账号保持连接。关闭面板不影响后台直播。</p>
  </div>
</template>

<style scoped>
.automatic-live,form,fieldset{display:grid;gap:12px;min-width:0}p,dl,dd{margin:0}fieldset{border:0;margin:0;padding:0}label{display:grid;gap:6px;font-size:13px}.hint{font-size:13px;line-height:1.65;color:var(--text-2,#b6b6ae)}.automatic-state{font-size:14px;font-weight:600;color:var(--accent,#dda951)}input,select,button{font:inherit;color:var(--text-0,#eeece6);border:1px solid var(--control-border,#7d7b75);border-radius:3px;background:var(--field,#191b1f);box-sizing:border-box;min-width:0}input,select{width:100%;padding:7px 8px;font-size:13px}button{padding:6px 10px;min-height:32px;cursor:pointer;background:var(--bg-2,#282a2f);justify-self:start;font-size:13px}button:hover:not(:disabled){border-color:var(--accent,#dda951)}button:disabled{opacity:.5;cursor:default}input:focus-visible,select:focus-visible,button:focus-visible{outline:2px solid var(--accent,#dda951);outline-offset:2px}input::placeholder{color:var(--text-2,#b6b6ae)}.primary{background:var(--accent,#dda951);border-color:var(--accent,#dda951);color:var(--accent-ink,#201d16);font-weight:600}.primary:hover:not(:disabled){background:var(--accent-2,#edbc6c)}.stop,.error{color:var(--danger,#ed9696)}.error{font-size:13px;line-height:1.6;overflow-wrap:anywhere}.row,.actions{display:flex;gap:8px;flex-wrap:wrap}.row>label{flex:1;min-width:100px}.check{display:flex;align-items:center;gap:6px}.check input{width:auto}.target-summary,.folder-preview,.activity{display:grid;gap:8px;border-top:1px solid var(--border,#3b3d42);padding-top:12px;font-size:13px;line-height:1.6}.target-summary p,dd,li{overflow-wrap:anywhere}.folder-preview ol{margin:0;padding-left:24px;max-height:200px;overflow:auto;scrollbar-color:var(--control-border,#7d7b75) var(--bg-1,#202226)}.folder-preview li{padding:2px 0}dl{display:grid;gap:6px}dl>div{display:grid;grid-template-columns:80px minmax(0,1fr);gap:8px}dt{color:var(--text-2,#b6b6ae)}dd{font-variant-numeric:tabular-nums}.automatic-live ::selection{background:var(--accent,#dda951);color:var(--accent-ink,#201d16)}input{caret-color:var(--accent,#dda951)}@media(pointer:coarse){button,input,select{min-height:44px}}@media(prefers-reduced-motion:reduce){*{transition:none}}
</style>
