<script setup>
import { onMounted, onBeforeUnmount, reactive, ref } from 'vue'
import { api } from '../../../web/src/api'

const settings = ref({ version: null, channels: [], default_channel: null }), records = ref([])
const busy = ref(false), error = ref(''), feedback = ref(''), editing = ref(false), tab = ref('channels')
const form = reactive({ id: '', name: '', kind: 'wecomlink', enabled: true, key: '' })
const message = reactive({ channel: '', title: 'Gamer 测试通知', content: '通知通道测试。' })
let timer, disposed = false
const call = (action, values = {}) => api.callExtension('gamer-notify', action, values)
const labels = { queued: '等待发送', sending: '发送中', sent: '接口已接受', partial: '部分成功', pending: '处理中', failed: '失败', unknown: '结果未知', skipped: '未发送' }
const sourceLabels = { task: '任务结果', script: '自动化步骤', manual: '手动发送', test: '通道测试' }
async function refresh() {
  const [channels, history] = await Promise.all([call('channels.read'), call('records.read')])
  if (disposed) return
  settings.value = channels; records.value = history.records || []
}
async function act(action, values) {
  if (busy.value) return null
  busy.value = true; error.value = ''; feedback.value = ''
  try { const result = await call(action, values); await refresh(); return result }
  catch (e) { error.value = e.message; return null }
  finally { busy.value = false }
}
function edit(channel) {
  Object.assign(form, channel ? { ...channel, key: '' } : { id: '', name: '', kind: 'wecomlink', enabled: true, key: '' })
  editing.value = true
}
async function save() {
  const id = form.id.trim() || (globalThis.crypto?.randomUUID?.() ?? `channel-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`)
  const result = await act('channels.save', { expected_version: settings.value.version, channel: { id, name: form.name.trim(), kind: form.kind, enabled: form.enabled, key: form.key } })
  if (result) { form.key = ''; editing.value = false; feedback.value = '通道已保存' }
}
async function toggle(channel) {
  await act('channels.save', { expected_version: settings.value.version, channel: { id: channel.id, name: channel.name, kind: channel.kind, enabled: !channel.enabled, key: '' } })
}
async function remove(channel) {
  const result = await act('channels.delete', { expected_version: settings.value.version, id: channel.id })
  if (result) feedback.value = '通道已删除；任务与脚本中的引用保留'
}
async function setDefault(channel) {
  await act('channels.default', { expected_version: settings.value.version, id: settings.value.default_channel === channel.id ? null : channel.id })
}
async function send(channel = null) {
  const result = await act('notification.send', { channel: channel?.id || message.channel || null, title: message.title, content: message.content, source: channel ? 'test' : 'manual' })
  if (result) { feedback.value = result.record.message; tab.value = 'records' }
}
async function query(record) {
  const result = await act('records.query', { id: record.id })
  if (result) feedback.value = result.record.message
}
async function poll() {
  try { if (!busy.value) { const history = await call('records.read'); if (!disposed) records.value = history.records || [] } }
  catch (e) { if (!disposed) error.value = e.message }
  if (!disposed) timer = setTimeout(poll, 2500)
}
onMounted(async () => { try { await refresh() } catch (e) { error.value = e.message } if (!disposed) timer = setTimeout(poll, 2500) })
onBeforeUnmount(() => { disposed = true; clearTimeout(timer); form.key = '' })
</script>

<template>
  <div class="notify-workspace">
    <div class="toolbar">
      <button type="button" class="btn btn-sm" :class="{ 'btn-primary': tab === 'channels' }" @click="tab = 'channels'">通知通道</button>
      <button type="button" class="btn btn-sm" :class="{ 'btn-primary': tab === 'records' }" @click="tab = 'records'">发送记录</button>
      <button type="button" class="btn btn-sm btn-ghost" :disabled="busy" @click="refresh().catch(e => error = e.message)">刷新</button>
    </div>
    <p v-if="error" class="error" role="alert">{{ error }}</p>
    <p v-if="feedback" class="feedback" role="status">{{ feedback }}</p>
    <template v-if="tab === 'channels'">
      <p class="hint">全局通道供任务和自动化共用。任务结果通知在任务编辑界面配置。</p>
      <button type="button" class="btn btn-primary btn-sm" @click="edit(null)">新增通道</button>
      <form v-if="editing" class="channel-form" @submit.prevent="save">
        <label>名称<input v-model="form.name" class="input" required maxlength="100" placeholder="例如：我的微信" /></label>
        <label>类型<select v-model="form.kind" class="select"><option value="wecomlink">企微连（微信通知）</option></select></label>
        <label>调用密钥<input v-model="form.key" class="input" type="password" autocomplete="new-password" :placeholder="settings.channels.some(c => c.id === form.id) ? '已配置，留空保留原密钥' : '请输入企微连调用密钥'" /></label>
        <details><summary>通道 ID</summary><input v-model="form.id" class="input mono" :disabled="settings.channels.some(c => c.id === form.id)" placeholder="留空自动生成；供脚本引用" /></details>
        <label class="inline"><input v-model="form.enabled" type="checkbox" />启用通道</label>
        <div class="toolbar"><button class="btn btn-primary btn-sm" :disabled="busy">保存</button><button type="button" class="btn btn-sm" @click="editing = false; form.key = ''">取消</button></div>
      </form>
      <div v-for="channel in settings.channels" :key="channel.id" class="channel">
        <div><strong>{{ channel.name }}</strong><span class="badge">{{ channel.enabled ? '已启用' : '已停用' }}</span><span v-if="settings.default_channel === channel.id" class="badge">默认</span></div>
        <code>{{ channel.id }}</code>
        <div class="toolbar">
          <button class="btn btn-sm btn-ghost" @click="edit(channel)">编辑</button>
          <button class="btn btn-sm btn-ghost" :disabled="busy" @click="toggle(channel)">{{ channel.enabled ? '停用' : '启用' }}</button>
          <button class="btn btn-sm btn-ghost" :disabled="busy || !channel.enabled" @click="setDefault(channel)">{{ settings.default_channel === channel.id ? '取消默认' : '设为默认' }}</button>
          <button class="btn btn-sm btn-ghost" :disabled="busy || !channel.enabled" @click="send(channel)">测试</button>
          <button class="btn btn-sm btn-ghost danger" :disabled="busy" @click="remove(channel)">删除</button>
        </div>
      </div>
      <p v-if="!settings.channels.length" class="hint">还没有通知通道。</p>
      <form class="channel-form" @submit.prevent="send()">
        <strong>手动发送</strong>
        <label>通道<select v-model="message.channel" class="select"><option value="">全局默认通道</option><option v-for="channel in settings.channels" :key="channel.id" :value="channel.id" :disabled="!channel.enabled">{{ channel.name }}</option></select></label>
        <label>标题<input v-model="message.title" class="input" /></label>
        <label>正文<textarea v-model="message.content" class="input" rows="3" required /></label>
        <p class="hint">标题、换行和正文合计最多 2048 个 UTF-8 字节；测试按钮使用这里的文案。</p>
        <button class="btn btn-primary btn-sm" :disabled="busy">发送</button>
      </form>
    </template>
    <template v-else>
      <p class="hint">“接口已接受”不代表手机已收到。结果未知时先核实接收端；不会自动重发。保留最近 30 天、最多 1000 条记录。</p>
      <div v-for="record in records" :key="record.id" class="record">
        <div><strong>{{ record.title || '无标题' }}</strong><span class="badge">{{ labels[record.status] || record.status }}</span></div>
        <div class="hint">{{ new Date(record.created_at).toLocaleString() }} · {{ sourceLabels[record.source] || record.source }} · {{ record.channel_name || record.channel_id || '默认通道' }}</div>
        <p class="content">{{ record.content }}</p><p class="hint">{{ record.message }}</p>
        <button v-if="record.remote_id && ['unknown', 'pending', 'partial'].includes(record.status)" class="btn btn-sm btn-ghost" :disabled="busy" @click="query(record)">查询结果</button>
      </div>
      <p v-if="!records.length" class="hint">暂无发送记录。</p>
    </template>
  </div>
</template>

<style scoped>
.notify-workspace { padding: 12px; overflow: auto; height: 100%; font-size: 13px; }
.toolbar { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; margin-bottom: 10px; }
.hint { font-size: 12px; color: var(--text-secondary, #777); line-height: 1.6; }
.channel-form { display: grid; gap: 10px; border: 1px solid var(--border, #ddd); padding: 12px; margin: 12px 0; border-radius: 6px; }
.channel-form label { display: grid; gap: 5px; }
.channel-form .inline { display: flex; align-items: center; }
.channel, .record { padding: 12px 0; border-bottom: 1px solid var(--border, #ddd); }
.channel code { display: block; font-size: 11px; margin: 5px 0; overflow-wrap: anywhere; }
.badge { font-size: 11px; margin-left: 8px; color: var(--text-secondary, #777); }
.content { white-space: pre-wrap; overflow-wrap: anywhere; }
.error { color: var(--danger, #c44); }.feedback { color: var(--success, #298555); }
</style>
