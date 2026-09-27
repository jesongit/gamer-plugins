<script setup>
import { computed, onMounted, onBeforeUnmount, reactive, ref } from 'vue'
import { api } from '../../../web/src/api'
import InteractionPanel from './InteractionPanel.vue'

const devices = ref([]), status = ref({ stream: null, connection: {} }), events = ref([])
const feedback = ref(''), savedSettings = ref({ version: null, profiles: {} })
const error = ref(''), busy = ref(false), gap = ref(false), copied = ref(false)
const activeTab = ref('settings')
const tabs = [{ key: 'settings', label: '直播设置' }, { key: 'rules', label: '互动规则' }, { key: 'logs', label: '触发日志' }]
const output = reactive({ device_id: '', mode: 'local', push_url: '', audio: true, fps: 30, bitrate_kbps: 4000 })
const credentials = reactive({ mode: 'open_live', access_key: '', access_secret: '', app_id: '', identity_code: '', access_token: '' })
const savedProfile = computed(() => savedSettings.value.profiles?.[credentials.mode])
function canReuse(field) { return !!savedProfile.value?.[`has_${field}`] && savedProfile.value.access_key === credentials.access_key && (field !== 'identity_code' || savedProfile.value.app_id === credentials.app_id) }
function fillProfile() { const saved = savedProfile.value || {}; credentials.access_key = saved.access_key || ''; credentials.app_id = saved.app_id || ''; credentials.access_secret = ''; credentials.identity_code = ''; credentials.access_token = '' }
function applySettings(value, selectMode = false) { savedSettings.value = value; if (selectMode && value.mode) credentials.mode = value.mode; fillProfile() }
function submittedCredentials() { const value = { ...credentials }; if (value.mode === 'open_live') value.access_token = ''; else { value.app_id = ''; value.identity_code = '' }; return value }
const streamActive = computed(() => status.value.stream && !['failed', 'stopped'].includes(status.value.stream.state))
const connected = computed(() => ['connecting', 'connected', 'reconnecting'].includes(status.value.connection.state))
const states = { preparing: '准备中', streaming: '输出中', connecting: '连接中', connected: '已连接', reconnecting: '正在重连', failed: '失败', stopped: '已停止', disconnected: '未连接' }
const kinds = { message: '弹幕', 'message.mirror': '镜像弹幕', gift: '礼物', super_chat: '醒目留言', membership: '舰长', like: '点赞', enter: '进入', follow: '关注', 'room.started': '开播', 'room.ended': '下播', 'message.removed': '留言删除' }
let timer, disposed = false, cursor = 0
const call = (action, values = {}) => api.callExtension('gamer-live', action, values)
async function refresh() {
  const snapshot = await call('live.status')
  const page = await call('events.read', { after: cursor })
  if (disposed) return
  status.value = snapshot
  if (streamActive.value) {
    for (const key of ['device_id', 'mode', 'audio', 'fps', 'bitrate_kbps']) {
      if (snapshot.stream[key] != null) output[key] = snapshot.stream[key]
    }
  }
  if (connected.value && snapshot.connection.mode) credentials.mode = snapshot.connection.mode
  if (page.gap) gap.value = true
  cursor = page.next_seq
  events.value = [...events.value, ...page.events].slice(-100)
}
async function poll() {
  try { if (!busy.value) await refresh() } catch (e) { if (!disposed) error.value = e.message }
  if (!disposed) timer = setTimeout(poll, 1500)
}
async function act(action, values = {}) {
  if (busy.value) return
  busy.value = true; error.value = ''; feedback.value = ''; copied.value = false
  try { const result = await call(action, values); await refresh(); return result || true } catch (e) { error.value = e.message; return false } finally { busy.value = false }
}
async function startOutput() {
  const ok = await act('stream.start', { ...output, push_url: output.mode === 'local' ? '' : output.push_url })
  if (ok) output.push_url = ''
}
async function connect() {
  const result = await act('connection.connect', { platform_id: 'bilibili', credentials: submittedCredentials(), expected_version: savedSettings.value.version })
  if (result) {
    if (result.settings) applySettings(result.settings)
    else { credentials.access_secret = ''; credentials.access_token = ''; credentials.identity_code = '' }
    feedback.value = '接入配置已保存，下次连接无需重复输入'
  }
}
async function saveSettings(clear = false) {
  if (busy.value) return
  busy.value = true; error.value = ''; feedback.value = ''
  try {
    const values = { expected_version: savedSettings.value.version, ...(clear ? { mode: credentials.mode } : { credentials: submittedCredentials() }) }
    const result = await call(clear ? 'connection.settings.clear' : 'connection.settings.save', values)
    applySettings(result)
    feedback.value = clear ? '已清除当前接入方式的配置' : '接入配置已保存，下次连接无需重复输入'
  } catch (e) { error.value = e.message } finally { busy.value = false }
}
async function copySource() {
  try { await navigator.clipboard.writeText(status.value.stream.source_url); copied.value = true } catch { error.value = '复制失败，请手动选择并复制素材地址' }
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
        <p class="connection-status" role="status">{{ states[status.stream?.state] || '未输出' }}</p>
        <p class="hint">设备画面与游戏声音交给直播姬，麦克风和开播在直播姬中设置。</p>
        <form @submit.prevent="startOutput">
          <fieldset :disabled="busy || streamActive">
            <label>设备<select v-model="output.device_id" aria-label="设备" required><option value="" disabled>选择设备</option><option v-for="d in devices" :key="d.id" :value="d.id">{{ d.name || d.id }}</option></select></label>
            <label>输出方式<select v-model="output.mode" aria-label="输出方式"><option value="local">直播姬多媒体素材（推荐）</option><option value="rtmp">手动 RTMP / RTMPS 推流</option></select></label>
            <label v-if="output.mode === 'rtmp'">完整推流地址<input v-model="output.push_url" type="password" autocomplete="off" required placeholder="rtmp://服务器/路径/推流码" /></label>
            <p v-if="output.mode === 'rtmp'" class="hint">开始后会直接向此地址发送画面和游戏声音。需要混音时，请使用上方的多媒体素材方式。</p>
            <div class="row"><label>帧率<input v-model.number="output.fps" type="number" min="10" max="60" required /></label><label>码率 Kbps<input v-model.number="output.bitrate_kbps" type="number" min="500" max="20000" required /></label></div>
            <label class="check"><input v-model="output.audio" type="checkbox" />包含游戏声音</label>
            <button type="submit" :disabled="!output.device_id">开始输出</button>
          </fieldset>
          <button v-if="streamActive" type="button" :disabled="busy" @click="act('stream.stop')">停止输出</button>
        </form>
        <div v-if="status.stream?.source_url" class="source">
          <label>本机素材地址<input :value="status.stream.source_url" readonly aria-label="本机素材地址" @focus="$event.target.select()" /></label>
          <button :disabled="!streamActive" @click="copySource">{{ copied ? '已复制' : '复制地址' }}</button>
          <p class="hint">直播姬添加「多媒体」素材并粘贴此地址。仅限运行 Gamer 服务端的这台电脑；首次出画面需要几秒，重启输出后需重新粘贴地址。</p>
        </div>
        <p v-if="status.stream" class="hint">输出设备：{{ devices.find(d => d.id === status.stream.device_id)?.name || status.stream.device_id }} · 视频 {{ status.stream.video_frames }} 帧 · 音频 {{ status.stream.audio_packets }} 包</p>
        <p v-if="status.stream?.error" class="error">{{ status.stream.error }}</p>
      </section>
        <section aria-label="互动连接">
          <h3>互动连接</h3>
          <p class="connection-status" role="status">{{ states[status.connection.state] || '未连接' }}</p>
          <p class="hint">接收弹幕、礼物、点赞等已获授权的事件。开发者账号还需申请对应直播权限；两种接入方式的凭据不能混用。</p>
          <form @submit.prevent="connect" autocomplete="off">
            <fieldset :disabled="busy || connected">
              <label>接入方式<select v-model="credentials.mode" aria-label="接入方式" @change="fillProfile"><option value="open_live">直播开放平台 · 主播身份码</option><option value="oauth">开放平台 · 已授权 OAuth Token</option></select></label>
              <label>Access Key ID<input v-model="credentials.access_key" aria-label="Access Key ID" required autocomplete="off" /></label>
              <label>Access Key Secret<input v-model="credentials.access_secret" aria-label="Access Key Secret" type="password" :required="!canReuse('access_secret')" :placeholder="canReuse('access_secret') ? '已保存，留空沿用；输入可更新' : '填写 Access Key Secret'" autocomplete="new-password" /></label>
              <template v-if="credentials.mode === 'open_live'"><label>应用 ID<input v-model="credentials.app_id" aria-label="应用 ID" required inputmode="numeric" /></label><label>主播身份码<input v-model="credentials.identity_code" aria-label="主播身份码" type="password" :required="!canReuse('identity_code')" :placeholder="canReuse('identity_code') ? '已保存，留空沿用；输入可更新' : '填写主播身份码'" autocomplete="off" /></label></template>
              <label v-else>Access Token<input v-model="credentials.access_token" aria-label="Access Token" type="password" :required="!canReuse('access_token')" :placeholder="canReuse('access_token') ? '已保存，留空沿用；输入可更新' : '填写 Access Token'" autocomplete="off" /></label>
              <div class="connection-actions"><button type="submit">保存并连接互动</button><button type="button" @click="saveSettings()">保存接入配置</button><button type="button" :disabled="!savedProfile" @click="saveSettings(true)">清除已保存配置</button></div>
            </fieldset>
            <button v-if="connected" type="button" :disabled="busy" @click="act('connection.disconnect')">断开互动</button>
          </form>
          <p class="hint">接入配置保存在服务端本机，断开和重启后保留，不随配置包导出。密钥框显示“已保存”时留空即可沿用；Token 失效后可在此更新。</p>
          <p class="hint">断开互动只停止接收消息，已排队操作继续执行。已排队操作可在「触发日志」中移除或取消；关闭规则开关可停止后续触发。</p>
          <p v-if="status.connection.error" class="error">{{ status.connection.error }}</p>
        </section>
      </div>
      <InteractionPanel :devices="devices" :view="activeTab" :connected="connected" />
      <div v-show="activeTab === 'logs'" class="interaction-content">
        <section>
          <div class="heading"><h3>最近互动</h3><button @click="events = []; gap = false">清空显示</button></div>
          <p v-if="gap" class="hint">连接期间有部分旧事件超出缓存，只显示仍保留的记录。</p>
          <p v-if="!events.length" class="hint">等待直播间事件。配置并启用互动规则后，新消息可进入执行队列。</p>
          <ol><li v-for="event in [...events].reverse()" :key="event.seq"><time>{{ new Date(event.received_at).toLocaleTimeString() }}</time><b>{{ kinds[event.kind] || event.kind }}</b><span>{{ event.actor?.name || '匿名观众' }}</span><span>{{ event.payload.text || event.payload.gift_name || '' }}{{ event.payload.count != null ? ` × ${event.payload.count}` : '' }}</span><span v-if="event.kind === 'gift'">礼物 ID：{{ event.payload.gift_id }}</span></li></ol>
        </section>
      </div>
      <p class="hint">关闭此面板不会停止连接；停用插件或退出 Gamer 会停止全部输出和互动。</p>
    </div>
  </div>
</template>

<style scoped>
.live-workspace{display:flex;flex:1;min-height:0;min-width:0;flex-direction:column;gap:8px;overflow:hidden;color:var(--text-0,#ddd)}
.workbench-tabs{display:flex;gap:4px;flex-shrink:0;border-bottom:1px solid var(--border,#41444c);padding-bottom:6px}
.workbench-tabs .tab-btn{height:28px;padding:3px 10px;border:1px solid transparent;border-radius:3px;background:transparent;color:var(--text-2,#aaa);font-size:13px}
.workbench-tabs .tab-btn:hover{color:var(--text-0,#ddd)}
.workbench-tabs .tab-btn.active{border-color:var(--border,#41444c);background:var(--bg-2,#292d35);color:var(--text-0,#ddd);font-weight:700}
.live-content{flex:1;min-height:0;overflow:auto;display:flex;flex-direction:column;gap:16px;padding:8px;scrollbar-gutter:stable}
.live-content>*{box-sizing:border-box;flex-shrink:0;width:100%;max-width:820px;margin-inline:auto}
.settings-content,.interaction-content{display:grid;gap:16px}
.connection-actions{display:flex;gap:8px;flex-wrap:wrap}
.connection-status{font-size:12px;color:#77cbb4}
h3,p{margin:0}section{display:grid;gap:12px}.hint{font-size:12px;opacity:.7;line-height:1.7}section{border:1px solid var(--border,#41444c);border-radius:10px;padding:16px;min-width:0}.heading,.row{display:flex;align-items:center;justify-content:space-between;gap:12px}.heading span{font-size:12px;color:#77cbb4}.row>label{flex:1;min-width:0}fieldset{border:0;padding:0;margin:0;min-width:0;display:grid;gap:12px}label{display:grid;gap:6px;font-size:13px}input,select{box-sizing:border-box;width:100%;min-width:0;padding:8px;border:1px solid var(--border,#535762);border-radius:5px;background:var(--bg-1,#24262c);color:inherit}button{padding:7px 12px;border-radius:5px;border:1px solid var(--border,#535762);background:var(--bg-2,#292d35);color:inherit;cursor:pointer;justify-self:start}button:disabled,fieldset:disabled{opacity:.55}form,.source{display:grid;gap:12px}.check{display:flex;align-items:center}.check input{width:auto}.error{color:#f19494;font-size:13px;overflow-wrap:anywhere}ol{padding:0;margin:0;list-style:none;max-height:320px;overflow:auto}li{display:flex;gap:8px;flex-wrap:wrap;padding:8px 0;border-bottom:1px solid var(--border,#41444c);font-size:12px;overflow-wrap:anywhere}time{opacity:.55}li b{color:#77cbb4}
</style>
