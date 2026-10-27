<script setup>
import { computed, nextTick, onMounted, onBeforeUnmount, reactive, ref, watch } from 'vue'
import { api } from '../../../web/src/api'
import AudienceSettings from './AudienceSettings.vue'
import InteractionPanel from './InteractionPanel.vue'
import AutomaticLivePanel from './AutomaticLivePanel.vue'
import UsersPanel from './UsersPanel.vue'
import SenderSettings from './SenderSettings.vue'

const devices = ref([]), status = ref({ stream: null, connection: {} })
const feedback = ref(''), savedSettings = ref({ version: null, profiles: {} })
const error = ref(''), busy = ref(false), copied = ref(false)
const automaticOperation = ref('')
const activeTab = ref('settings')
const outputKind = ref('device'), interactionQueue = ref({ waiting: [] })
const tabs = [{ key: 'settings', label: '直播输出' }, { key: 'connection', label: '互动接入' }, { key: 'feedback', label: '反馈提醒' }, { key: 'rules', label: '互动规则' }, { key: 'users', label: '用户管理' }, { key: 'logs', label: '触发日志' }]
const output = reactive({ device_id: '', mode: 'local', push_url: '', audio: true, fps: 30, bitrate_kbps: 4000 })
const credentials = reactive({ mode: 'open_live', access_key: '', access_secret: '', app_id: '', identity_code: '', access_token: '' })
const savedProfile = computed(() => savedSettings.value.profiles?.[credentials.mode])
function canReuse(field) { return !!savedProfile.value?.[`has_${field}`] && savedProfile.value.access_key === credentials.access_key && (field !== 'identity_code' || savedProfile.value.app_id === credentials.app_id) }
function fillProfile() { const saved = savedProfile.value || {}; credentials.access_key = saved.access_key || ''; credentials.app_id = saved.app_id || ''; credentials.access_secret = ''; credentials.identity_code = ''; credentials.access_token = '' }
function applySettings(value, selectMode = false) { savedSettings.value = value; if (selectMode && value.mode) credentials.mode = value.mode; fillProfile() }
function submittedCredentials() { const value = { ...credentials }; if (value.mode === 'open_live') value.access_token = ''; else { value.app_id = ''; value.identity_code = '' }; return value }
const canOutput = computed(() => devices.value.find(d => d.id === output.device_id)?.capabilities?.media_output !== false)
const streamActive = computed(() => status.value.stream && !['failed', 'stopped'].includes(status.value.stream.state))
const sourceUrl = computed(() => status.value.stream?.source_url || (output.mode === 'local' ? status.value.local_sources?.stream_url : '') || '')
const automatic = computed(() => status.value.automatic || { state: 'stopped' })
const automaticActive = computed(() => !['stopped', 'failed'].includes(automatic.value.state || 'stopped'))
watch(automaticActive, active => { if (active) outputKind.value = 'automatic' })
const connected = computed(() => ['connecting', 'connected', 'reconnecting'].includes(status.value.connection.state))
const states = { preparing: '准备中', streaming: '输出中', connecting: '连接中', connected: '已连接', reconnecting: '正在重连', failed: '失败', stopped: '已停止', disconnected: '未连接' }
let timer, disposed = false, outputModeRestored = false, pendingActions = 0, actionSequence = 0, refreshSequence = 0
const call = (action, values = {}) => api.callExtension('gamer-live', action, values)
async function refresh() {
  const sequence = ++refreshSequence
  const snapshot = await call('live.status')
  if (disposed || sequence !== refreshSequence) return
  status.value = snapshot
  if (!outputModeRestored) {
    if (automatic.value.video_folder && (!streamActive.value || automaticActive.value)) outputKind.value = 'automatic'
    outputModeRestored = true
  }
  if (streamActive.value) {
    for (const key of ['device_id', 'mode', 'audio', 'fps', 'bitrate_kbps']) {
      if (key === 'device_id' && automaticActive.value) continue
      if (snapshot.stream[key] != null) output[key] = snapshot.stream[key]
    }
  }
  if (connected.value && snapshot.connection.mode) credentials.mode = snapshot.connection.mode
}
async function poll() {
  try { if (!busy.value || automaticOperation.value) await refresh() } catch (e) { if (!disposed) error.value = e.message }
  if (!disposed) timer = setTimeout(poll, 1500)
}
async function act(action, values = {}) {
  if (busy.value && action !== 'automatic.stop') return
  const sequence = ++actionSequence
  pendingActions++
  if (action.startsWith('automatic.')) automaticOperation.value = action
  busy.value = true; error.value = ''; feedback.value = ''; copied.value = false
  try { const result = await call(action, values); await refresh(); return result || true }
  catch (e) { if (sequence === actionSequence) error.value = e.message; return false }
  finally { pendingActions--; busy.value = pendingActions > 0; if (!busy.value) automaticOperation.value = '' }
}
async function startOutput() {
  if (automaticActive.value) return
  if (!canOutput.value) { error.value = '当前目标尚不支持音视频输出'; return }
  const ok = await act('stream.start', { ...output, push_url: output.mode === 'local' ? '' : output.push_url })
  if (ok) output.push_url = ''
}
async function showExecutionTarget() {
  activeTab.value = 'connection'
  await nextTick()
  const target = document.getElementById('live-execution-target')
  target?.scrollIntoView({ block: 'nearest', behavior: 'auto' })
  target?.querySelector('select')?.focus()
}
async function connect() {
  const result = await act('connection.connect', { platform_id: 'bilibili', credentials: submittedCredentials(), expected_version: savedSettings.value.version })
  if (result) {
    if (result.settings) applySettings(result.settings)
    else { credentials.access_secret = ''; credentials.access_token = ''; credentials.identity_code = '' }
    feedback.value = '接入配置已保存'
  }
}
async function saveSettings(clear = false) {
  if (busy.value) return
  busy.value = true; error.value = ''; feedback.value = ''
  try {
    const values = { expected_version: savedSettings.value.version, ...(clear ? { mode: credentials.mode } : { credentials: submittedCredentials() }) }
    const result = await call(clear ? 'connection.settings.clear' : 'connection.settings.save', values)
    applySettings(result)
    feedback.value = clear ? '接入配置已清除' : '接入配置已保存'
  } catch (e) { error.value = e.message } finally { busy.value = false }
}
async function copySource() {
  try { await navigator.clipboard.writeText(sourceUrl.value); copied.value = true } catch { error.value = '复制失败，请手动选择并复制素材地址' }
}
onMounted(async () => {
  try { devices.value = await api.listDevices(); if (!output.device_id) output.device_id = devices.value[0]?.id || '' } catch (e) { error.value = e.message }
  try { const saved = await call('connection.settings.read'); if (!disposed) applySettings(saved, true) } catch (e) { error.value = e.message }
  if (!disposed) poll()
})
onBeforeUnmount(() => { disposed = true; clearTimeout(timer) })
</script>

<template>
  <div class="live-workspace">
    <nav class="workbench-tabs" aria-label="直播助手功能">
      <button v-for="tab in tabs" :key="tab.key" type="button" class="tab-btn" :class="{ active: activeTab === tab.key }" :aria-pressed="activeTab === tab.key" :aria-controls="`live-${tab.key}`" @click="activeTab = tab.key">{{ tab.label }}</button>
    </nav>
    <div class="live-content">
      <p v-if="error" role="alert" class="error">{{ error }}</p><p v-if="feedback" role="status" class="connection-status">{{ feedback }}</p>
      <div v-show="activeTab === 'settings'" id="live-settings" class="settings-content">
      <section aria-label="音视频输出">
        <h3>音视频输出</h3>
        <label>直播模式<select v-model="outputKind" aria-label="直播模式" :disabled="busy || streamActive || automaticActive"><option value="device">设备直播</option><option value="automatic">自动直播 · 视频与弹幕互动</option></select></label>
        <AutomaticLivePanel v-if="outputKind === 'automatic'" :status="automatic" :queue="interactionQueue" :devices="devices" :output="output" :busy="busy" :perform="act" @configure-target="showExecutionTarget" @open-rules="activeTab = 'rules'" @open-logs="activeTab = 'logs'" />
        <template v-else>
        <p class="connection-status" role="status">{{ states[status.stream?.state] || '未输出' }}</p>
        <form @submit.prevent="startOutput">
          <fieldset :disabled="busy || streamActive">
            <label>设备<select v-model="output.device_id" aria-label="设备" required><option value="" disabled>选择设备</option><option v-for="d in devices" :key="d.id" :value="d.id">{{ d.name || d.id }}</option></select></label>
            <label>输出方式<select v-model="output.mode" aria-label="输出方式"><option value="local">直播姬多媒体素材（推荐）</option><option value="rtmp">手动 RTMP / RTMPS 推流</option></select></label>
            <label v-if="output.mode === 'rtmp'">完整推流地址<input v-model="output.push_url" type="password" autocomplete="off" required /></label>
            <p v-if="output.mode === 'rtmp'" class="hint">开始后直接推送画面与声音。</p>
            <details class="output-options"><summary>画质设置</summary><div class="row"><label>帧率<input v-model.number="output.fps" type="number" min="10" max="60" required /></label><label>码率 Kbps<input v-model.number="output.bitrate_kbps" type="number" min="500" max="20000" required /></label></div></details>
            <label class="check"><input v-model="output.audio" type="checkbox" />包含游戏声音</label>
            <button type="submit" :disabled="!output.device_id || !canOutput" :title="canOutput ? '' : '当前目标尚不支持音视频输出'">开始输出</button>
          </fieldset>
          <button v-if="streamActive" type="button" :disabled="busy" @click="act('stream.stop')">停止输出</button>
        </form>
        </template>
        <div v-if="sourceUrl" class="source">
          <label>本机素材地址<input :value="sourceUrl" readonly aria-label="本机素材地址" @focus="$event.target.select()" /></label>
          <button @click="copySource">{{ copied ? '已复制' : '复制地址' }}</button>
          <p class="hint">同机直播姬 → 多媒体素材。地址固定，开始输出后可读取。</p>
        </div>
        <p v-if="status.stream && !automaticActive" class="hint">输出设备：{{ devices.find(d => d.id === status.stream.device_id)?.name || status.stream.device_id }} · 视频 {{ status.stream.video_frames }} 帧 · 音频 {{ status.stream.audio_packets }} 包</p>
        <p v-if="status.stream?.error" class="error">{{ status.stream.error }}</p>
        <p v-if="status.stream?.device_id?.startsWith('browser-')" class="hint">浏览器输出仅包含画面，声音为静音。</p>
      </section>
      </div>
      <div v-show="activeTab === 'connection'" id="live-connection" class="settings-content">
        <section aria-label="互动连接">
          <h3>互动连接</h3>
          <p class="connection-status" role="status">{{ states[status.connection.state] || '未连接' }}</p>
          <form @submit.prevent="connect" autocomplete="off">
            <fieldset class="credential-grid" :disabled="busy || connected">
              <label class="full-field">接入方式<select v-model="credentials.mode" aria-label="接入方式" @change="fillProfile"><option value="open_live">直播开放平台 · 主播身份码</option><option value="oauth">开放平台 · OAuth Token</option></select></label>
              <label>Access Key ID<input v-model="credentials.access_key" aria-label="Access Key ID" required autocomplete="off" /></label>
              <label>Access Key Secret<input v-model="credentials.access_secret" aria-label="Access Key Secret" type="password" :required="!canReuse('access_secret')" :placeholder="canReuse('access_secret') ? '已保存' : undefined" autocomplete="new-password" /></label>
              <template v-if="credentials.mode === 'open_live'"><label>应用 ID<input v-model="credentials.app_id" aria-label="应用 ID" required inputmode="numeric" /></label><label>主播身份码<input v-model="credentials.identity_code" aria-label="主播身份码" type="password" :required="!canReuse('identity_code')" :placeholder="canReuse('identity_code') ? '已保存' : undefined" autocomplete="off" /></label></template>
              <label v-else>Access Token<input v-model="credentials.access_token" aria-label="Access Token" type="password" :required="!canReuse('access_token')" :placeholder="canReuse('access_token') ? '已保存' : undefined" autocomplete="off" /></label>
              <p v-if="savedProfile" class="hint full-field">密钥留空保留，填写后更新。</p>
              <div class="connection-actions full-field"><button type="submit">保存并连接</button><button type="button" @click="saveSettings()">保存配置</button><button type="button" :disabled="!savedProfile" @click="saveSettings(true)">清除配置</button></div>
            </fieldset>
            <button v-if="connected" type="button" :disabled="busy" @click="act('connection.disconnect')">断开互动</button>
          </form>
          <p v-if="connected" class="hint">断开互动后，已排队任务继续执行。</p>
          <p v-if="status.connection.error" class="error">{{ status.connection.error }}</p>
        </section>
      </div>
      <InteractionPanel v-show="['connection', 'rules', 'logs'].includes(activeTab)" :devices="devices" :view="activeTab === 'connection' ? 'settings' : activeTab === 'settings' ? 'output' : activeTab" :connected="connected" :connection-state="status.connection.state" :connection-error="status.connection.error" :automatic-active="automaticActive" @queue-change="interactionQueue = $event" />
      <div v-show="activeTab === 'feedback'" id="live-feedback" class="settings-content"><AudienceSettings /><SenderSettings /></div>
      <UsersPanel v-if="activeTab === 'users'" />
    </div>
  </div>
</template>

<style scoped>
.live-workspace{display:flex;flex:1;min-height:0;min-width:0;flex-direction:column;gap:8px;overflow:hidden;color:var(--text-0,#ddd)}
.workbench-tabs{display:flex;gap:4px;flex-shrink:0;flex-wrap:wrap;border-bottom:1px solid var(--border,#41444c);padding-bottom:6px}
.workbench-tabs .tab-btn{height:28px;padding:3px 10px;border:1px solid transparent;border-radius:3px;background:transparent;color:var(--text-2,#aaa);font-size:13px}
.workbench-tabs .tab-btn:hover{color:var(--text-0,#ddd)}
.workbench-tabs .tab-btn.active{border-color:var(--border,#41444c);background:var(--bg-2,#292d35);color:var(--text-0,#ddd);font-weight:700}
.live-content{flex:1;min-height:0;overflow:auto;display:flex;flex-direction:column;gap:10px;padding:6px;scrollbar-gutter:stable}
.live-content>*{box-sizing:border-box;flex-shrink:0;width:100%;max-width:820px;margin-inline:auto}
.settings-content,.interaction-content{display:grid;gap:10px}
.credential-grid{grid-template-columns:repeat(2,minmax(0,1fr))}.full-field{grid-column:1/-1}.output-options summary{cursor:pointer;font-size:13px}.output-options[open]>summary{margin-bottom:8px}
.connection-actions{display:flex;gap:8px;flex-wrap:wrap}
.connection-status{font-size:12px;color:#77cbb4}
h3,p{margin:0}section{display:grid;gap:8px}.hint{font-size:12px;color:var(--text-2,#b6b6ae);line-height:1.5}section{border:1px solid var(--border,#41444c);border-radius:4px;padding:10px;min-width:0}.heading,.row{display:flex;align-items:center;justify-content:space-between;gap:8px}.heading span{font-size:12px;color:#77cbb4}.row>label{flex:1;min-width:0}fieldset{border:0;padding:0;margin:0;min-width:0;display:grid;gap:8px}label{display:grid;gap:4px;font-size:13px}input,select{box-sizing:border-box;width:100%;min-width:0;padding:6px 8px;border:1px solid var(--border,#535762);border-radius:3px;background:var(--bg-1,#24262c);color:inherit}button{padding:5px 9px;border-radius:3px;border:1px solid var(--border,#535762);background:var(--bg-2,#292d35);color:inherit;cursor:pointer;justify-self:start}button:disabled,fieldset:disabled{opacity:.55}form,.source{display:grid;gap:8px}.check{display:flex;align-items:center}.check input{width:auto}.error{color:#f19494;font-size:13px;overflow-wrap:anywhere}ol{padding:0;margin:0;list-style:none;max-height:320px;overflow:auto}li{display:flex;gap:8px;flex-wrap:wrap;padding:8px 0;border-bottom:1px solid var(--border,#41444c);font-size:12px;overflow-wrap:anywhere}time{opacity:.55}li b{color:#77cbb4}
@media(max-width:360px){.credential-grid{grid-template-columns:1fr}}
@media(pointer:coarse){button,input,select{min-height:40px}}
</style>
