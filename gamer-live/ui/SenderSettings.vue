<script setup>
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import { api } from '../../../web/src/api'
const labels = { accepted: '加入队列', quota_rejected: '额度不足', scan_required: '等待扫码', confirmation_required: '等待观众确认', rescan_requested: '重新扫码', cancelled: '观众取消', timeout: '登录超时', success: '任务成功', failed: '任务失败' }
const saved = ref({ enabled: false, configured: false, has_session: false, has_csrf: false, version: null })
const draft = reactive({ enabled: false, room_id: '', session_cookie: '', csrf_token: '', templates: Object.fromEntries(Object.keys(labels).map(key => [key, ''])) })
const loading = ref(true), loaded = ref(false), busy = ref(false), error = ref(''), feedback = ref(''), message = ref('')
const canSend = computed(() => saved.value.enabled && saved.value.configured)
const messageLength = computed(() => Array.from(message.value).length)
const invalidTemplates = computed(() => Object.values(draft.templates).some(value => Array.from(value).length > 200 || /\p{Cc}/u.test(value)))
let disposed = false
const call = (action, values = {}) => api.callExtension('gamer-live', action, values)
function apply(value) {
  saved.value = value; draft.enabled = value.enabled === true; draft.room_id = String(value.room_id || '')
  draft.session_cookie = ''; draft.csrf_token = ''
  for (const key of Object.keys(labels)) draft.templates[key] = value.templates?.[key] ?? ''
}
async function load() {
  loading.value = true; error.value = ''
  try { const value = await call('sender.read'); if (!disposed) { apply(value); loaded.value = true } }
  catch (e) { if (!disposed) error.value = `主动弹幕设置未读取：${e.message}` }
  finally { if (!disposed) loading.value = false }
}
async function save() {
  if (busy.value || !loaded.value) return
  if (invalidTemplates.value) { error.value = '提醒文字每项最多 200 字符，且不能换行。'; return }
  busy.value = true; error.value = ''; feedback.value = ''
  try {
    const values = { enabled: draft.enabled, room_id: draft.room_id, expected_version: saved.value.version, templates: { ...draft.templates } }
    if (draft.session_cookie) values.session_cookie = draft.session_cookie
    if (draft.csrf_token) values.csrf_token = draft.csrf_token
    apply(await call('sender.save', values)); feedback.value = '主动弹幕设置已保存。'
  } catch (e) { error.value = `设置未保存：${e.message}` }
  finally { busy.value = false }
}
async function clear() {
  if (busy.value) return
  busy.value = true; error.value = ''; feedback.value = ''
  try { apply(await call('sender.clear', { expected_version: saved.value.version })); feedback.value = '已清除发言账号，主动弹幕已停用。' }
  catch (e) { error.value = `账号未清除：${e.message}` }
  finally { busy.value = false }
}
async function send() {
  if (busy.value || !canSend.value || !message.value.trim() || messageLength.value > 33) return
  busy.value = true; error.value = ''; feedback.value = ''
  try { await call('sender.send', { text: message.value, request_id: crypto.randomUUID() }); message.value = ''; feedback.value = '弹幕已提交发送队列。' }
  catch (e) { error.value = `弹幕未提交：${e.message}` }
  finally { busy.value = false }
}
async function refreshStatus() {
  if (busy.value) return
  busy.value = true; error.value = ''
  try { const value = await call('sender.read'); saved.value = { ...saved.value, enabled: value.enabled, configured: value.configured, account_name: value.account_name, last_error: value.last_error }; feedback.value = '发言状态已更新。' }
  catch (e) { error.value = `状态未读取：${e.message}` }
  finally { busy.value = false }
}
onMounted(load)
onBeforeUnmount(() => { disposed = true })
</script>

<template>
  <section class="sender-settings" aria-label="主动弹幕">
    <div class="head"><h3>主动弹幕</h3><span class="hint">{{ saved.account_name || (saved.configured ? '发言凭据已保存' : '尚未设置发言账号') }}</span></div>
    <p v-if="loading" class="hint" role="status">正在读取设置…</p>
    <form v-if="loaded" @submit.prevent="save" autocomplete="off"><fieldset :disabled="busy">
      <label class="check"><input v-model="draft.enabled" type="checkbox" role="switch" aria-label="启用主动弹幕" />启用自动提醒</label>
      <label>直播间 ID<input v-model="draft.room_id" aria-label="发言直播间 ID" inputmode="numeric" /></label>
      <details :open="!saved.configured"><summary>发言账号{{ saved.configured ? ' · 已保存' : '' }}</summary><div class="credential-fields">
      <label>SESSDATA<input v-model="draft.session_cookie" type="password" aria-label="SESSDATA" autocomplete="new-password" :placeholder="saved.has_session ? '已保存' : undefined" /></label>
      <label>bili_jct<input v-model="draft.csrf_token" type="password" aria-label="bili_jct" autocomplete="new-password" :placeholder="saved.has_csrf ? '已保存' : undefined" /></label>
      </div><p class="hint">单独配置发言账号；凭据留空保留。</p></details>
      <details><summary>自动提醒文字</summary><p class="hint">留空关闭对应提醒。可用：{viewer}、{number}、{position}、{cost}、{remaining}、{available}。</p><div class="template-fields"><label v-for="(label, key) in labels" :key="key">{{ label }}<input v-model="draft.templates[key]" :aria-label="`${label}提醒文字`" maxlength="200" /></label></div></details>
      <div class="actions"><button type="submit" :disabled="invalidTemplates">保存发言设置</button><button type="button" :disabled="!saved.has_session && !saved.has_csrf" @click="clear">清除发言账号</button></div>
    </fieldset></form>
    <form v-if="loaded" class="manual-send" @submit.prevent="send"><label>手动发送弹幕<input v-model="message" aria-label="手动弹幕内容" maxlength="33" :aria-invalid="messageLength > 33" :disabled="busy || !canSend" placeholder="填写要发送到直播间的文字" /></label><div class="actions"><button type="submit" :disabled="busy || !canSend || !message.trim() || messageLength > 33">提交发送</button><span class="hint">{{ messageLength }} / 33 · 自动带【Gamer】前缀</span></div><p v-if="!canSend" class="hint">保存完整的发言账号并启用后，可手动发送。</p></form>
    <div v-if="loaded" class="actions"><button type="button" :disabled="busy" @click="refreshStatus">刷新发言状态</button><span class="hint">{{ saved.enabled ? (saved.configured ? '主动弹幕已启用' : '账号设置尚不完整') : '主动弹幕未启用' }}</span></div>
    <p v-if="saved.last_error" class="error" role="status">最近发言未成功：{{ saved.last_error }}</p>
    <p v-if="feedback" class="feedback" role="status">{{ feedback }}</p><p v-if="error" class="error" role="alert">{{ error }}</p><button v-if="!loaded && !loading" type="button" @click="load">重新读取</button>
  </section>
</template>

<style scoped>
.sender-settings{display:grid;gap:8px;min-width:0;padding:10px;border:1px solid var(--border,#3b3d42);border-radius:4px;color:var(--text-0,#eeece6);font-size:13px}.head,.actions{display:flex;align-items:center;gap:8px;justify-content:space-between;flex-wrap:wrap}.actions{justify-content:start}h3,p{margin:0}h3{font-size:14px}.hint{font-size:12px;line-height:1.6;color:var(--text-2,#b6b6ae)}form,fieldset,label{display:grid;gap:6px;min-width:0}form,fieldset{gap:8px}fieldset{margin:0;padding:0;border:0}input,textarea,button{box-sizing:border-box;min-width:0;max-width:100%;border:1px solid var(--control-border,#7d7b75);border-radius:3px;font:inherit;color:inherit;padding:5px 8px}input,textarea{width:100%;background:var(--field,#191b1f);caret-color:var(--accent,#dda951)}textarea{resize:vertical;line-height:1.5}button{background:var(--bg-3,#34363b);justify-self:start;cursor:pointer}button:hover:not(:disabled){background:var(--border,#3b3d42)}button:disabled,input:disabled,fieldset:disabled{opacity:.55;cursor:default}.check{display:flex;gap:8px;align-items:center}.check input{width:28px;height:20px;accent-color:var(--accent,#dda951);margin:0}.manual-send{border-top:1px solid var(--border,#3b3d42);padding-top:12px}details{display:grid;min-width:0}summary{cursor:pointer;font-weight:500}details>*+*{margin-top:8px}.feedback{color:var(--ok,#8fc9ae)}.error{color:var(--danger,#ed9696);overflow-wrap:anywhere}:is(input,textarea,button,summary):focus-visible{outline:2px solid var(--accent,#dda951);outline-offset:2px}::selection{background:var(--accent,#dda951);color:var(--accent-ink,#201d16)}
.credential-fields,.template-fields{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:8px}.manual-send{padding-top:8px}
@media(max-width:360px){.credential-fields,.template-fields{grid-template-columns:1fr}}
</style>
