<script setup>
import { computed, inject, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue'
import { api } from '../../../web/src/api'
import { WORKSPACE_CONTEXT_KEY } from '../../../web/src/workspace/context'
import { PROTOCOLS, budgetValue, chatTimeline, displayTime, eventDetails, eventImage, isActive, pauseGuidance, stateLabel, tokenUsage, usageValue } from './ai-format'

const workspace = inject(WORKSPACE_CONTEXT_KEY, null)
const emit = defineEmits(['session-start'])
const context = computed(() => workspace?.getSnapshot?.() || {})
const deviceId = computed(() => context.value.deviceId || '')
const packageId = computed(() => context.value.currentPackageId || '')
const deviceName = computed(() => context.value.device?.name || deviceId.value || '未选择设备')
const settingsSection = ref(''), timelineElement = ref(null), nearBottom = ref(true), newConversation = ref(false)
const saved = ref(null), sessions = ref([]), selectedId = ref(''), tokens = ref([])
const busy = ref(''), controlBusy = ref(''), messageBusy = ref(false), error = ref(''), feedback = ref(''), statusFresh = ref(false)
const goal = ref(''), mode = ref('api')
const limits = reactive({ max_turns: 40, max_actions: 120, max_seconds: 600, max_tokens: 100000, max_failures: 3 })
const limitFields = [
  { key: 'max_turns', label: '最大模型轮数（0 表示无上限）', min: 0, nonZeroMin: 1, max: 500 },
  { key: 'max_actions', label: '最大工具次数（0 表示无上限）', min: 0, nonZeroMin: 1, max: 2000 },
  { key: 'max_seconds', label: '最长活动时长（秒，0 表示无上限）', min: 0, nonZeroMin: 10, max: 7200 },
  { key: 'max_tokens', label: '累计 token 上限（0 表示无上限）', min: 0, nonZeroMin: 2048, max: 2000000 },
  { key: 'max_failures', label: '连续失败上限（0 表示无上限）', min: 0, nonZeroMin: 1, max: 20 },
]
const settings = reactive({ base_url: PROTOCOLS.responses.baseUrl, model: 'glm-5.3-flash', protocol: 'responses', request_timeout_secs: 60, public_reasoning_content: false, api_key: '' })
const tokenForm = reactive({ label: '', control: false, ttl_seconds: 120, device_scope: 'selected', memory_read: false, memory_write: false, protected_write: false, web_search: false })
const createdToken = ref(null), copied = ref('')
const mcpEndpoint = `${globalThis.location?.origin || 'http://127.0.0.1:8443'}/api/extensions/gamer-ai/mcp`
const selectedSession = computed(() => newConversation.value ? null : sessions.value.find(s => s.session_id === selectedId.value)
  || sessions.value.find(s => s.device_id === deviceId.value && isActive(s))
  || sessions.value.find(s => s.device_id === deviceId.value) || null)
const currentActive = computed(() => sessions.value.find(s => s.device_id === deviceId.value && isActive(s)))
const controlSession = computed(() => sessions.value.find(s => s.device_id === deviceId.value
  && s.content_package === packageId.value && s.mode === 'mcp' && isActive(s)))
const goalBytes = computed(() => new TextEncoder().encode(goal.value.trim()).length)
const canStart = computed(() => !!deviceId.value && !!packageId.value && !!goal.value.trim() && goalBytes.value <= 8000
  && statusFresh.value && !currentActive.value && (mode.value === 'mcp' || !!saved.value?.has_key))
const boundElsewhere = computed(() => selectedSession.value && (selectedSession.value.device_id !== deviceId.value || selectedSession.value.content_package !== packageId.value))
const canFollowUp = computed(() => statusFresh.value && !boundElsewhere.value && selectedSession.value?.mode === 'api'
  && ['running', 'paused'].includes(selectedSession.value?.state) && !!goal.value.trim() && goalBytes.value <= 8000)
const canSend = computed(() => !messageBusy.value && !controlBusy.value && limitsValid.value
  && (isActive(selectedSession.value) ? canFollowUp.value : canStart.value && !busy.value))
const sendLabel = computed(() => !isActive(selectedSession.value) ? (mode.value === 'mcp' ? '建立外部控制会话' : '发送目标')
  : selectedSession.value.state === 'paused' ? '发送并继续' : '发送新指令')
const limitsChanged = computed(() => !!selectedSession.value && limitFields.some(field => limits[field.key] !== selectedSession.value.limits?.[field.key]))
const limitsValid = computed(() => limitFields.every(field => Number.isInteger(limits[field.key])
  && limits[field.key] <= field.max && (limits[field.key] === 0 || limits[field.key] >= field.nonZeroMin)))
const hardUsage = computed(() => ({ max_turns: selectedSession.value?.usage?.turns || 0,
  max_actions: selectedSession.value?.usage?.actions || 0, max_seconds: selectedSession.value?.usage?.active_seconds || 0,
  max_tokens: selectedSession.value?.usage?.total_tokens ?? selectedSession.value?.usage?.known_tokens ?? 0 }))
const budgetBlocked = computed(() => Object.entries(hardUsage.value).some(([key, value]) => limits[key] > 0 && value >= limits[key]))
const canPause = computed(() => statusFresh.value && ['starting', 'running', 'resuming'].includes(selectedSession.value?.state))
const canResume = computed(() => statusFresh.value && selectedSession.value?.state === 'paused' && limitsValid.value && !budgetBlocked.value)
const timeline = computed(() => chatTimeline(selectedSession.value))
const clientConfig = computed(() => JSON.stringify({ mcpServers: { gamer: { url: mcpEndpoint,
  headers: { Authorization: `Bearer ${createdToken.value?.token || '<在面板中创建的连接令牌>'}` } } } }, null, 2))
const probe = computed(() => saved.value?.probe)
const probeLabels = { model: '模型连接', image_input: '图片识别', function_calling: '基于图片的工具调用', tool_image_feedback: '工具截图反馈', tool_result_roundtrip: '工具结果回传' }
const probeChecks = computed(() => {
  const checks = probe.value?.checks
  if (Array.isArray(checks)) return checks.map((item, i) => typeof item === 'string'
    ? { key: String(i), label: item, ok: probe.value.ok }
    : { ...item, key: item.name || item.id || String(i), label: item.label || probeLabels[item.name || item.id] || item.name || item.id || `检查 ${i + 1}` })
  return checks && typeof checks === 'object' ? Object.entries(checks).map(([key, value]) => ({
    key, label: key, ...(typeof value === 'object' ? value : { ok: !!value }),
  })) : []
})
const call = (action, values = {}) => api.callExtension('gamer-ai', action, values)
let timer, disposed = false

function applySettings(value) {
  saved.value = value
  for (const key of ['base_url', 'model', 'protocol', 'request_timeout_secs']) if (value?.[key] != null) settings[key] = value[key]
  settings.public_reasoning_content = settings.protocol === 'chat_completions' && value?.public_reasoning_content === true
  settings.api_key = ''
}
async function refresh() {
  const result = await call('session.get')
  if (disposed) return
  sessions.value = result?.sessions || []
  statusFresh.value = true
  if (!sessions.value.some(s => s.session_id === selectedId.value)) selectedId.value = (
    sessions.value.find(s => s.device_id === deviceId.value && isActive(s))
    || sessions.value.find(s => s.device_id === deviceId.value) || sessions.value[0]
  )?.session_id || ''
}
async function refreshTokens() {
  const result = await call('mcp.tokens.list')
  if (!disposed) tokens.value = result?.tokens || []
}
async function poll() {
  try { await refresh() } catch (e) {
    if (!disposed) { statusFresh.value = false; error.value = e.message || '无法读取 AI 状态' }
  }
  if (!disposed) timer = setTimeout(poll, 1500)
}
async function operate(label, fn) {
  if (busy.value) return
  busy.value = label; error.value = ''; feedback.value = ''; copied.value = ''
  try { await fn() } catch (e) { if (!disposed) error.value = e.message || '操作失败' }
  finally { if (!disposed) busy.value = '' }
}
async function start() {
  if (!canStart.value || !limitsValid.value) return
  await operate('正在开始', async () => {
    const result = await call('session.start', { device_id: deviceId.value, content_package: packageId.value,
      goal: goal.value.trim(), mode: mode.value, limits: { ...limits } })
    selectedId.value = result?.session?.session_id || result?.session_id || ''
    if(result?.conversation_id) emit('session-start',{conversation_id:result.conversation_id})
    newConversation.value = false
    goal.value = ''
    await refresh()
    feedback.value = mode.value === 'mcp' ? '外部控制会话已建立；客户端可使用匹配目标的控制令牌调用工具。' : 'AI 会话已提交。'
  })
}
async function sendMessage(resume = true) {
  if (!canSend.value) return
  if (resume && (!limitsValid.value || (selectedSession.value?.state === 'paused' && budgetBlocked.value))) return
  if (!isActive(selectedSession.value)) return start()
  const session = selectedSession.value, message = goal.value.trim()
  messageBusy.value = true; error.value = ''; feedback.value = ''
  try {
    const reply = await call('session.message', { session_id: session.session_id, message, resume,
      ...(limitsChanged.value ? { limits: { ...limits } } : {}) })
    if (reply?.session) sessions.value = sessions.value.map(item => item.session_id === session.session_id ? reply.session : item)
    goal.value = ''
    feedback.value = reply?.resumed ? '新指令已发送，AI 将使用最新画面继续。' : '消息已接收，会话保持暂停。'
    if (reply?.resume_error) error.value = `消息已接收，恢复未完成：${typeof reply.resume_error === 'string' ? reply.resume_error : reply.resume_error.message || '请检查暂停原因'}`
    try { await refresh() } catch (e) { statusFresh.value = false; error.value = `消息已接收，但状态刷新失败：${e.message || '请刷新会话'}` }
  } catch (e) { if (!disposed) error.value = `${e.message || '发送结果未确认'}；请刷新会话确认后再决定是否重试。` }
  finally { if (!disposed) messageBusy.value = false }
}
function toggleSettings(section) { settingsSection.value = settingsSection.value === section ? '' : section }
watch(() => settings.protocol, protocol => { if (protocol !== 'chat_completions') settings.public_reasoning_content = false })
function selectConversation(value) { newConversation.value = false; selectedId.value = value }
function clearConversation() { if (!currentActive.value) { newConversation.value = true; selectedId.value = ''; goal.value = '' } }
function onTimelineScroll() {
  const el = timelineElement.value
  if (el) nearBottom.value = el.scrollHeight - el.clientHeight - el.scrollTop < 80
}
watch(() => `${selectedSession.value?.session_id}:${timeline.value.map(item => `${item.key}:${item.seq || ''}`).join('|')}`, async (value, previous) => {
  if (value.split(':')[0] !== previous?.split(':')[0]) nearBottom.value = true
  if (!nearBottom.value) return
  await nextTick()
  if (timelineElement.value) timelineElement.value.scrollTop = timelineElement.value.scrollHeight
})
watch(() => `${selectedSession.value?.session_id}:${selectedSession.value?.state}`, () => {
  if (!selectedSession.value) return
  for (const field of limitFields) if (selectedSession.value.limits?.[field.key] != null) limits[field.key] = selectedSession.value.limits[field.key]
}, { immediate: true })
async function control(action) {
  const session = selectedSession.value
  if (!session || controlBusy.value || (action === 'resume' && !canResume.value)) return
  controlBusy.value = action === 'pause' ? '正在请求暂停' : action === 'resume' ? '正在请求恢复' : '正在请求停止'
  error.value = ''; feedback.value = ''
  try {
    const reply = await call(`session.${action}`, { session_id: session.session_id,
      ...(action === 'resume' && limitsChanged.value ? { limits: { ...limits } } : {}) })
    if (reply?.session) sessions.value = sessions.value.map(item => item.session_id === session.session_id ? reply.session : item)
    await refresh()
  } catch (e) { if (!disposed) error.value = e.message || '会话控制失败' }
  finally { if (!disposed) controlBusy.value = '' }
}
async function saveSettings(test = false) {
  await operate(test ? '保存并测试连接' : '保存连接设置', async () => {
    const values = { expected_version: saved.value?.version ?? null, base_url: settings.base_url.trim(),
      model: settings.model.trim(), protocol: settings.protocol, request_timeout_secs: settings.request_timeout_secs,
      public_reasoning_content: settings.protocol === 'chat_completions' && settings.public_reasoning_content }
    if (settings.api_key.trim()) values.api_key = settings.api_key.trim()
    applySettings(await call('settings.save', values))
    feedback.value = '连接设置已保存。'
    if (test) await probeSaved()
  })
}
async function probeSaved() {
  const result = await call('connection.probe')
  const snapshot = await call('settings.get')
  const report = snapshot?.probe || result?.probe || result
  applySettings({ ...snapshot, probe: report })
  feedback.value = report?.ok ? '图片与工具闭环测试通过。' : '连接能力测试未通过，请查看检查结果。'
}
async function testConnection() { await operate('测试已保存的连接', probeSaved) }
async function createToken() {
  await operate('创建连接令牌', async () => {
    createdToken.value = await call('mcp.tokens.create', { device_id: deviceId.value, content_package: packageId.value,
      label: tokenForm.label.trim(), control: tokenForm.control, ttl_seconds: tokenForm.ttl_seconds,
      ...(tokenForm.device_scope === 'none' ? {device_id:''} : {}),
      memory_read:tokenForm.memory_read,memory_write:tokenForm.memory_write,
      protected_write:tokenForm.protected_write,web_search:tokenForm.web_search })
    await refreshTokens()
    feedback.value = '连接令牌已创建。完整令牌只在此次创建结果中显示。'
  })
}
async function revokeToken(token) {
  await operate('撤销连接令牌', async () => {
    await call('mcp.tokens.revoke', { token_id: token.token_id })
    if (createdToken.value?.token_id === token.token_id) createdToken.value = null
    await Promise.all([refreshTokens(), refresh()])
    feedback.value = token.control ? '控制令牌已撤销，关联外部控制会话将暂停；重新连接后仍需用户恢复。' : '只读令牌已撤销。'
  })
}
async function copy(value, label) {
  try { await navigator.clipboard.writeText(value); copied.value = label } catch { error.value = '无法复制，请手动选择文本复制。' }
}
function sessionTitle(session) { return `${session.device_id} · ${session.mode === 'mcp' ? '外部 MCP' : '内置 AI'} · ${stateLabel(session.state)}` }
function controlHint(session) {
  if (session?.state === 'paused') return '暂停完成，可以人工操作。继续 AI 前将收回控制权并重新截图；其他自动化须先停止本会话。'
  if (['pausing', 'stopping'].includes(session?.state)) return '正在收尾已入场的动作，完成前人工操作仍被锁定。'
  if (isActive(session)) return 'AI 持有控制权，人工操作已锁定。需要操作设备时，请先暂停并等待“已暂停”。'
  return '会话已结束，人工控制已开放。'
}
onMounted(async () => {
  const results = await Promise.allSettled([call('settings.get'), refresh(), refreshTokens()])
  if (disposed) return
  if (results[0].status === 'fulfilled') applySettings(results[0].value)
  const failed = results.find(result => result.status === 'rejected')
  if (failed) error.value = failed.reason?.message || '读取 AI 配置失败'
  if (!disposed) timer = setTimeout(poll, 1500)
})
onBeforeUnmount(() => { disposed = true; clearTimeout(timer); settings.api_key = ''; createdToken.value = null })
</script>

<template>
  <div class="ai-workspace">
    <header class="agent-header">
      <div class="agent-title"><span class="agent-avatar" aria-hidden="true">AI</span><h3>Agent</h3><span class="model-label">{{ saved?.model || '配置模型' }}</span></div>
      <div class="header-actions">
        <button type="button" :disabled="!!currentActive" title="新会话不会删除历史记录" @click="clearConversation">新会话</button>
        <button type="button" aria-controls="ai-settings" :aria-expanded="settingsSection === 'settings'" @click="toggleSettings('settings')">模型</button>
        <button type="button" aria-controls="ai-mcp" :aria-expanded="settingsSection === 'mcp'" @click="toggleSettings('mcp')">MCP</button>
        <button type="button" aria-controls="ai-budget" :aria-expanded="settingsSection === 'budget'" @click="toggleSettings('budget')">预算</button>
      </div>
    </header>
    <div class="scope-line"><span>{{ deviceName }}</span><span>{{ packageId || '未选配置包' }}</span><span v-if="context.androidPackageName">{{ context.androidPackageName }}</span></div>
    <div v-if="selectedSession" class="session-toolbar">
      <span class="session-state" role="status" data-testid="session-state">{{ stateLabel(selectedSession.state) }}</span>
      <div class="actions">
        <button v-if="canPause" type="button" :disabled="!!controlBusy || messageBusy" @click="control('pause')">暂停 AI</button>
        <button v-if="selectedSession.state === 'paused'" type="button" :disabled="!canResume || !!controlBusy || messageBusy" @click="control('resume')">继续 AI</button>
        <button v-if="isActive(selectedSession)" type="button" :disabled="!!controlBusy" @click="control('stop')">停止会话</button>
      </div>
    </div>
    <div class="conversation-selector" v-if="sessions.length">
      <label>会话<select :value="newConversation ? '' : selectedSession?.session_id || ''" aria-label="查看会话" @change="selectConversation($event.target.value)"><option v-if="newConversation" value="">新会话</option><option v-for="session in sessions" :key="session.session_id" :value="session.session_id">{{ sessionTitle(session) }}</option></select></label>
      <button type="button" @click="refresh().catch(e => error = e.message)">刷新</button>
    </div>
    <p v-if="boundElsewhere" class="scope-warning">此会话绑定设备 {{ selectedSession.device_id }} / 配置包 {{ selectedSession.content_package }}。切换工作台不会改写会话；请返回对应上下文发送消息，或新建当前目标的会话。</p>
    <div v-show="!!settingsSection" class="settings-drawer">
      <div v-show="settingsSection === 'settings'" id="ai-settings" class="section-stack">
        <section aria-label="模型连接设置">
          <h3>模型连接</h3>
          <p class="hint">连接设置保存在运行 Gamer 服务端的电脑，密钥不随配置包导出。协议切换仅在显式保存后生效。</p>
          <form autocomplete="off" @submit.prevent="saveSettings(false)"><fieldset :disabled="!!busy">
            <label>API 协议<select v-model="settings.protocol" aria-label="API 协议"><option value="responses">Responses</option><option value="chat_completions">Chat Completions</option></select></label>
            <label><input v-model="settings.public_reasoning_content" type="checkbox" aria-label="显示供应商公开思考" :disabled="settings.protocol !== 'chat_completions'" />显示供应商公开思考（Chat Completions）</label><p class="hint">默认关闭。开启后展示供应商 API 公开返回的 reasoning_content，适用于智谱等支持此字段的模型；不会读取隐藏推理。Responses 只展示公开摘要。</p>
            <label>API 基础地址<input v-model="settings.base_url" aria-label="API 基础地址" type="url" required placeholder="https://open.bigmodel.cn/api/v1" /></label>
            <div class="endpoint-tip"><span class="hint">智谱 {{ PROTOCOLS[settings.protocol]?.label }} 地址：{{ PROTOCOLS[settings.protocol]?.baseUrl }}</span><button type="button" @click="settings.base_url = PROTOCOLS[settings.protocol].baseUrl">填入此地址</button></div>
            <label>模型名称<input v-model="settings.model" aria-label="模型名称" required placeholder="glm-5.3-flash" /></label>
            <label>API 密钥<input v-model="settings.api_key" aria-label="API 密钥" type="password" autocomplete="new-password" :required="!saved?.has_key" :placeholder="saved?.has_key ? '已保存，留空沿用；输入可更新' : '输入 API 密钥'" /></label>
            <label>请求超时（秒）<input v-model.number="settings.request_timeout_secs" aria-label="请求超时（秒）" type="number" min="5" max="600" step="1" required /></label>
            <div class="actions"><button type="submit">保存连接设置</button><button type="submit" class="primary" @click.prevent="saveSettings(true)">保存并测试连接</button><button type="button" :disabled="!saved?.has_key" @click="testConnection">测试已保存配置</button></div>
          </fieldset></form>
          <p class="hint">测试使用合成图片和无设备副作用的工具，验证看图、工具调用和结果回传；不操作游戏。</p>
          <p v-if="saved" class="hint">已保存协议：{{ PROTOCOLS[saved.protocol]?.label || saved.protocol }} · 模型：{{ saved.model }} · 密钥：{{ saved.has_key ? '已保存' : '未设置' }}</p>
        </section>
        <section v-if="probe" aria-label="连接测试结果">
          <div class="heading"><h3>连接测试</h3><span :class="probe.ok ? 'feedback' : 'error'">{{ probe.ok ? '通过' : '未通过' }}</span></div>
          <p class="hint">{{ PROTOCOLS[probe.protocol]?.label || probe.protocol }} · {{ probe.model }} · {{ displayTime(probe.checked_at) }}</p>
          <ul class="checks"><li v-for="check in probeChecks" :key="check.key"><span :class="check.ok ? 'feedback' : 'error'">{{ check.ok ? '✓' : '✕' }}</span><b>{{ check.label }}</b><span>{{ check.message || check.error || check.detail || '' }}</span></li></ul>
          <p v-if="probe.error || probe.message" :class="probe.ok ? 'hint' : 'error'">{{ probe.error || probe.message }}</p>
          <p v-if="!probe.ok" class="hint">可显式选择另一协议及对应地址，保存后再次测试。运行期间不会自动切换协议。</p>
        </section>
      </div>
      <div v-show="settingsSection === 'mcp'" id="ai-mcp" class="section-stack">
        <section aria-label="MCP 接入">
          <h3>本机 MCP</h3>
          <p class="hint">使用 Streamable HTTP，仅接受运行 Gamer 服务端电脑上的客户端连接。独立 Bearer 令牌不使用管理登录 Cookie；可撤销，并有有效期。</p>
          <label>MCP 地址<input :value="mcpEndpoint" aria-label="MCP 地址" readonly @focus="$event.target.select()" /></label>
          <button type="button" @click="copy(mcpEndpoint, '地址')">{{ copied === '地址' ? '已复制地址' : '复制地址' }}</button>
          <p class="hint">若当前页面通过其他电脑或非回环地址打开，请在服务端电脑上改用相同端口的 localhost 地址连接。</p>
          <div class="context-line"><span>授权设备：{{ deviceName }}</span><span>配置包：{{ packageId || '未选择' }}</span></div>
          <form @submit.prevent="createToken"><fieldset :disabled="!!busy">
            <label>令牌备注<input v-model="tokenForm.label" aria-label="令牌备注" maxlength="80" placeholder="例如：本机 AI 客户端" /></label>
            <label>授权范围<select v-model="tokenForm.control" aria-label="授权范围"><option :value="false">只读观察 · 上下文与截图</option><option :value="true">观察与操作 · 仅已建立的外部会话</option></select></label>
            <label>设备绑定<select v-model="tokenForm.device_scope" aria-label="令牌设备绑定" @change="tokenForm.device_scope === 'none' && (tokenForm.control = false)"><option value="selected">当前设备</option><option value="none">不绑定设备 · 仅记忆与已授权联网</option></select></label>
            <label class="token-check"><input v-model="tokenForm.memory_read" type="checkbox" @change="!tokenForm.memory_read && (tokenForm.memory_write = tokenForm.protected_write = false)" />允许读取本配置包记忆</label>
            <label class="token-check"><input v-model="tokenForm.memory_write" type="checkbox" @change="tokenForm.memory_write ? (tokenForm.memory_read = true) : (tokenForm.protected_write = false)" />允许 AI 维护本配置包记忆</label>
            <label class="token-check"><input v-model="tokenForm.protected_write" type="checkbox" :disabled="!tokenForm.memory_write" />明确允许修改人工保护字段</label>
            <label class="token-check"><input v-model="tokenForm.web_search" type="checkbox" />允许使用已配置的独立联网服务</label>
            <p class="hint">记忆维护需要读取，勾选维护会同时允许读取；取消读取会取消维护和保护字段修改。联网与设备控制分别授权。记忆工具可在无设备或游戏暂停时使用，不能恢复游戏。</p>
            <label>控制租约超时（秒）<input v-model.number="tokenForm.ttl_seconds" aria-label="控制租约超时（秒）" type="number" min="30" max="3600" step="1" required /></label>
            <button type="submit" :disabled="!packageId || (tokenForm.device_scope === 'selected' && !deviceId) || (tokenForm.device_scope === 'none' && !(tokenForm.memory_read || tokenForm.memory_write || tokenForm.web_search)) || (tokenForm.control && (tokenForm.device_scope === 'none' || !controlSession))">创建连接令牌</button>
          </fieldset></form>
          <p v-if="tokenForm.control && !controlSession" class="hint">请先在新会话的“预算”中选择“外部 AI”，并建立当前设备与配置包的控制会话。</p>
          <p v-if="controlSession" class="hint">外部控制会话：{{ stateLabel(controlSession.state) }}。控制客户端仍须遵守暂停状态，不能自行恢复或更换目标。</p>
          <template v-if="createdToken"><label>新连接令牌<input :value="createdToken.token" type="password" readonly aria-label="新连接令牌" @focus="$event.target.select()" /></label><div class="actions"><button type="button" @click="copy(createdToken.token, '令牌')">{{ copied === '令牌' ? '已复制令牌' : '复制令牌' }}</button><button type="button" @click="createdToken = null">收起令牌</button></div><p class="hint">令牌有效期 24 小时；控制租约 {{ createdToken.ttl_seconds }} 秒。客户端须在租约期限内发送有效工具请求或 ping 续租；失联、令牌到期或撤销会暂停，需要用户恢复。</p></template>
          <details :open="!!createdToken"><summary>客户端配置示例</summary><pre>{{ clientConfig }}</pre><button type="button" @click="copy(clientConfig, '配置')">{{ copied === '配置' ? '已复制配置' : '复制配置' }}</button><p class="hint">示例适用于支持 URL 与 headers 的 MCP 客户端，请按客户端要求填写。完整令牌只在创建时提供，不会写入配置包或浏览器存储。</p></details>
        </section>
        <section aria-label="连接令牌列表">
          <div class="heading"><h3>连接令牌</h3><button type="button" :disabled="!!busy" @click="operate('刷新令牌', refreshTokens)">刷新</button></div>
          <ul v-if="tokens.length" class="token-list"><li v-for="token in tokens" :key="token.token_id"><div><b>{{ token.label || token.token_id }}</b><p class="hint">{{ token.device_id || '无设备' }} · {{ token.content_package }} · {{ token.control ? '观察与操作' : '无设备操作权限' }}<span v-if="token.memory_read"> · 记忆读取</span><span v-if="token.memory_write"> · 记忆维护</span><span v-if="token.protected_write"> · 人工保护修改</span><span v-if="token.web_search"> · 联网</span></p><p class="hint">到期：{{ token.expires_at ? new Date(typeof token.expires_at === 'number' && token.expires_at < 1e12 ? token.expires_at * 1000 : token.expires_at).toLocaleString('zh-CN') : '未返回' }}</p></div><button type="button" :disabled="!!busy" @click="revokeToken(token)">撤销</button></li></ul>
          <p v-else class="hint">暂无连接令牌。</p>
        </section>
      </div>

      <section v-show="settingsSection === 'budget'" id="ai-budget" class="section-stack" aria-label="运行预算">
        <h3>运行预算</h3>
        <p class="hint">所有预算均可设为 0（无上限），用量仍会累计。暂停时可提高上限或设为 0，再明确继续；只有修改过的预算才会提交。继续时重新计算连续失败次数。</p>
        <fieldset :disabled="isActive(selectedSession) && selectedSession.state !== 'paused'">
          <label>控制方式<select v-model="mode" aria-label="控制方式" :disabled="isActive(selectedSession)"><option value="api">内置 AI · 模型 API</option><option value="mcp">外部 AI · MCP 客户端</option></select></label>
          <div class="budget-grid"><label v-for="field in limitFields" :key="field.key">{{ field.label }}<input v-model.number="limits[field.key]" :aria-label="field.label" :data-budget="field.key" type="number" :min="field.min" :max="field.max" step="1" required /><small class="hint">0 无上限；非零范围 {{ field.nonZeroMin.toLocaleString('zh-CN') }}–{{ field.max.toLocaleString('zh-CN') }}</small></label></div>
        </fieldset>
        <p v-if="!limitsValid" class="error">运行预算超出允许范围，请修正后发送或继续。</p>
        <dl v-if="selectedSession" class="usage-grid"><div><dt>模型轮数</dt><dd>{{ usageValue(selectedSession.usage, ['turns']) }} / {{ budgetValue(selectedSession.limits, 'max_turns') }}</dd></div><div><dt>工具调用</dt><dd>{{ usageValue(selectedSession.usage, ['actions']) }} / {{ budgetValue(selectedSession.limits, 'max_actions') }}</dd></div><div><dt>活动秒数</dt><dd>{{ usageValue(selectedSession.usage, ['active_seconds']) }} / {{ budgetValue(selectedSession.limits, 'max_seconds') }}</dd></div><div><dt>累计 token</dt><dd>{{ tokenUsage(selectedSession.usage) }} / {{ budgetValue(selectedSession.limits, 'max_tokens') }}</dd><small v-if="selectedSession.usage?.has_unknown_tokens">部分请求用量未知</small></div><div><dt>连续失败</dt><dd>{{ usageValue(selectedSession.usage, ['consecutive_failures']) }} / {{ budgetValue(selectedSession.limits, 'max_failures') }}</dd></div></dl>
        <button v-if="selectedSession?.state === 'paused'" type="button" :disabled="!limitsChanged || !limitsValid || budgetBlocked || !!controlBusy || messageBusy" @click="control('resume')">调整预算并继续</button>
        <p v-if="budgetBlocked && selectedSession?.state === 'paused'" class="error">当前上限已耗尽，请提高对应预算或设为 0（无上限）。累计用量不会重置。</p>
      </section>
    </div>
    <main id="ai-play" class="agent-chat">
      <div ref="timelineElement" class="chat-scroll" @scroll="onTimelineScroll">
        <div v-if="!selectedSession" class="chat-empty"><span class="agent-avatar" aria-hidden="true">AI</span><h3>描述你希望完成的事情</h3><p>AI 会观察画面并操作。你可以继续发送指令来调整方向，随时暂停接管。</p><p v-if="!deviceId || !packageId">请先在工作台选择设备和配置包。</p><button v-if="mode === 'api' && !saved?.has_key" type="button" @click="toggleSettings('settings')">配置模型连接</button></div>
        <ol v-else class="events chat-timeline" role="log" aria-label="对话与执行进度" aria-live="polite">
          <li v-for="item in timeline" :key="item.key" :class="['message', `message-${item.kind}`]">
            <div class="message-heading"><b>{{ item.kind === 'user' ? '你' : item.kind === 'assistant' ? 'AI' : item.kind === 'decision' ? '公开决策说明' : item.kind === 'pause' ? item.pause?.title || 'AI 已暂停' : item.kind === 'tool' ? item.label : ['capture', 'observation'].includes(item.kind) ? '观察画面' : item.kind === 'error' ? '执行遇到问题' : '执行进度' }}</b><time>{{ displayTime(item.at) }}</time><span v-if="item.kind === 'tool'" class="tool-status" :class="item.status">{{ item.status === 'running' ? '执行中' : ['error', 'failed'].includes(item.status) ? '失败' : '完成' }}</span></div>
            <p class="message-text">{{ item.message }}</p>
            <template v-if="item.kind === 'pause'">
              <p v-if="item.current && selectedSession.usage?.known_tokens > 0 && selectedSession.usage?.total_tokens == null" class="pause-usage">累计 token：{{ tokenUsage(selectedSession.usage) }} / {{ budgetValue(selectedSession.limits, 'max_tokens') }}<span v-if="selectedSession.usage?.has_unknown_tokens">（部分请求用量未知）</span></p>
              <p class="pause-next">{{ pauseGuidance({ reason: item.message, pause_reason: item.pause }) }}</p>
              <button v-if="selectedSession.state === 'paused' && /预算|token|上限/i.test(item.message)" type="button" @click="settingsSection = 'budget'">调整运行预算</button>
            </template>
            <details v-if="!['user', 'decision'].includes(item.kind) && (eventDetails(item.data) || eventImage(item))" class="message-details"><summary>{{ eventImage(item) ? '查看参数、结果与截图' : '查看参数与结果' }}</summary><pre v-if="eventDetails(item.data)">{{ eventDetails(item.data) }}</pre><figure v-if="eventImage(item)"><img :src="eventImage(item)" alt="此次操作关联的目标截图" /><figcaption>此次观察的画面，当前投屏可能已变化。</figcaption></figure></details>
          </li>
        </ol>
        <p v-if="selectedSession?.state === 'running'" class="live-progress"><span class="status-dot" aria-hidden="true"></span>会话进行中，等待 AI 回复或下一项执行进度。</p>
      </div>
      <div class="composer-area">
        <p v-if="error" role="alert" class="error">{{ error }}</p><p v-if="feedback" role="status" class="feedback">{{ feedback }}</p>
        <p v-if="busy" role="status" class="hint">{{ busy }}…</p><p v-if="controlBusy || messageBusy" role="status" class="hint">{{ controlBusy || '正在发送新指令' }}…</p>
        <p v-if="selectedSession" class="hint control-hint" data-testid="control-hint">{{ controlHint(selectedSession) }}</p>
        <p v-if="!statusFresh" class="error">会话状态尚未同步；请刷新后发送，仍可请求停止。</p>
        <form class="chat-composer" @submit.prevent="sendMessage(true)">
          <textarea v-model="goal" aria-label="消息" rows="3" maxlength="8000" :disabled="messageBusy" placeholder="描述目标，或补充新的指令…" @keydown.ctrl.enter.prevent="sendMessage(true)" @keydown.meta.enter.prevent="sendMessage(true)" />
          <div class="composer-footer"><span>{{ selectedSession?.mode === 'mcp' || (!selectedSession && mode === 'mcp') ? '外部 MCP' : 'Agent' }}<span v-if="goalBytes > 8000" class="error"> · 超过 8000 字节</span></span><div class="actions"><button v-if="selectedSession?.state === 'paused' && selectedSession.mode === 'api'" type="button" :disabled="!canSend" @click="sendMessage(false)">仅发送，保持暂停</button><button type="submit" class="primary" :disabled="!canSend || (selectedSession?.state === 'paused' && budgetBlocked)">{{ sendLabel }}</button></div></div>
        </form>
        <p v-if="selectedSession?.state === 'running' && selectedSession.mode === 'api'" class="hint">发送新指令会打断本轮，等待当前动作收尾后，按新指令继续。不会更换会话目标。</p>
        <p v-else-if="selectedSession?.state === 'paused' && selectedSession.mode === 'api'" class="hint">发送并继续会收回人工控制；也可以仅发送，保持暂停。预算用尽时先提高上限或设为 0。</p>
        <p v-if="selectedSession?.mode === 'mcp' && isActive(selectedSession)" class="hint">外部会话的后续指令由 MCP 客户端发送；这里可查看真实执行进度并暂停/停止。</p>
        <p v-if="mode === 'mcp' && !isActive(selectedSession)" class="hint">先发送目标建立会话，再打开 MCP 设置创建控制令牌。</p>
        <p class="hint persistent-note">关闭面板不会停止会话。显示公开回答和真实操作，不展示私密推理过程。</p>
      </div>
    </main>
  </div>
</template>

<style scoped>
.token-check{display:flex;align-items:center;gap:8px}.token-check input{width:auto}
.conversation-selector label{white-space:nowrap}
.ai-workspace{display:flex;flex:1;min-height:0;min-width:0;flex-direction:column;overflow:hidden;color:var(--text-0,#edf0ee);position:relative;background:var(--bg-1,#181b1c)}
.agent-header{display:flex;justify-content:space-between;align-items:center;gap:8px;padding:10px 12px;border-bottom:1px solid var(--border,#454b4e);flex-wrap:wrap;flex-shrink:0}.agent-title{display:flex;align-items:center;gap:8px;min-width:0}.agent-avatar{display:inline-grid;place-items:center;width:27px;height:27px;border:1px solid var(--border,#454b4e);border-radius:7px;font-size:10px;font-weight:700;color:var(--accent,#e4c956);background:var(--bg-2,#282b2d)}h3{font-size:13px;margin:0}.model-label{font-size:11px;color:var(--text-2,#c5cbc8);overflow:hidden;text-overflow:ellipsis;white-space:nowrap;max-width:160px}.header-actions,.actions{display:flex;gap:5px;flex-wrap:wrap}.header-actions button{padding:4px 7px;font-size:11px;background:transparent}.header-actions button[aria-expanded=true]{color:var(--accent,#e4c956);border-color:var(--accent,#e4c956)}
.scope-line{display:flex;gap:6px 14px;flex-wrap:wrap;padding:6px 12px;font-size:10px;color:var(--text-2,#c5cbc8);overflow-wrap:anywhere;flex-shrink:0}.session-toolbar{display:flex;align-items:center;justify-content:space-between;gap:8px;padding:4px 12px 8px;flex-wrap:wrap;flex-shrink:0}.session-state{font-size:11px;color:var(--accent,#e4c956)}.conversation-selector{display:flex;gap:6px;align-items:center;padding:0 12px 6px}.conversation-selector label{display:flex;align-items:center;gap:6px;flex:1;min-width:0;font-size:10px;color:var(--text-2,#c5cbc8)}.conversation-selector select{font-size:10px;padding:4px}.scope-warning{margin:0;padding:7px 12px;font-size:11px;line-height:1.6;color:var(--accent,#e4c956);background:var(--bg-2,#282b2d)}
.settings-drawer{flex-shrink:0;max-height:44%;min-height:0;overflow:auto;padding:8px 12px;border-block:1px solid var(--border,#454b4e);background:var(--bg-2,#282b2d)}.section-stack,section,fieldset{display:grid;gap:10px;min-width:0}section+section{margin-top:12px}fieldset{border:0;padding:0;margin:0}label{display:grid;gap:5px;font-size:11px}input,select,textarea{box-sizing:border-box;width:100%;min-width:0;padding:7px;border:1px solid var(--border,#454b4e);border-radius:4px;background:var(--bg-1,#181b1c);color:inherit;font:inherit}form{display:grid;gap:8px}button{padding:5px 9px;border-radius:4px;border:1px solid var(--border,#454b4e);background:var(--bg-2,#282b2d);color:inherit;cursor:pointer;justify-self:start;font-size:11px}button.primary{color:var(--accent,#e4c956);border-color:var(--accent,#e4c956)}button:hover:not(:disabled){border-color:var(--accent,#e4c956)}button:disabled,fieldset:disabled{opacity:.5;cursor:default}button:focus-visible,input:focus-visible,select:focus-visible,textarea:focus-visible,summary:focus-visible{outline:2px solid var(--accent,#e4c956);outline-offset:2px}.budget-grid,.usage-grid{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:10px}.usage-grid{margin:0;font-size:11px}.usage-grid dt{color:var(--text-2,#c5cbc8)}.usage-grid dd{margin:4px 0}.usage-grid small{font-size:10px;color:var(--text-2,#c5cbc8)}.context-line{display:flex;gap:8px;flex-wrap:wrap;font-size:11px}.heading{display:flex;gap:8px;align-items:center;justify-content:space-between}.endpoint-tip{display:grid;gap:6px}
.agent-chat{display:flex;flex:1;min-height:0;flex-direction:column}.chat-scroll{flex:1;min-height:0;overflow:auto;padding:14px 12px;scrollbar-gutter:stable}.chat-empty{display:grid;gap:12px;justify-items:start;margin:20px 8px;color:var(--text-2,#c5cbc8);font-size:12px;line-height:1.7}.chat-empty .agent-avatar{width:36px;height:36px}.chat-timeline{list-style:none;margin:0;padding:0;display:grid;gap:16px}.message{min-width:0;font-size:12px;line-height:1.7;overflow-wrap:anywhere}.message-heading{display:flex;align-items:center;gap:8px;font-size:10px;color:var(--text-2,#c5cbc8);margin-bottom:5px}.message-heading b{font-size:11px;color:var(--text-0,#edf0ee)}.message-heading time{font-size:9px;margin-left:auto}.message-text{margin:0;white-space:pre-wrap}.message-user{margin-left:18px;padding:10px 12px;border:1px solid var(--border,#454b4e);border-radius:8px;background:var(--bg-2,#282b2d)}.message-decision,.message-progress,.message-model,.message-state{border-left:2px solid var(--border,#454b4e);padding-left:10px;color:var(--text-2,#c5cbc8)}.message-tool,.message-observation,.message-capture{border:1px solid var(--border,#454b4e);border-radius:6px;padding:9px 11px;background:var(--bg-2,#282b2d)}.tool-status{font-size:9px;color:#77cbb4}.tool-status.running{color:var(--accent,#e4c956)}.tool-status.error,.tool-status.failed{color:var(--danger,#ef9292)}.message-pause{border:1px solid var(--accent,#e4c956);border-radius:6px;padding:12px;background:var(--bg-2,#282b2d)}.message-pause .message-heading b{color:var(--accent,#e4c956)}.pause-next{margin:8px 0;font-size:11px;color:var(--text-2,#c5cbc8)}.pause-usage{margin:8px 0 0;font-size:11px;color:var(--accent,#e4c956)}.message-error{border-left:2px solid var(--danger,#ef9292);padding-left:10px;color:var(--danger,#ef9292)}summary{cursor:pointer;font-size:10px;color:var(--text-2,#c5cbc8)}.message-details{margin-top:7px}pre{margin:8px 0 0;font-size:10px;line-height:1.6;white-space:pre-wrap;overflow-wrap:anywhere;background:var(--bg-1,#181b1c);padding:9px;border-radius:4px;max-height:240px;overflow:auto}figure{margin:9px 0 0;display:grid;gap:5px}figure img{max-width:100%;max-height:300px;object-fit:contain;border-radius:4px}figcaption{font-size:10px;color:var(--text-2,#c5cbc8)}.live-progress{margin:14px 0 0;font-size:10px;color:var(--text-2,#c5cbc8);display:flex;align-items:center;gap:7px}.status-dot{width:5px;height:5px;border-radius:50%;background:var(--accent,#e4c956)}
.composer-area{flex-shrink:0;padding:10px 12px;border-top:1px solid var(--border,#454b4e);display:grid;gap:6px;background:var(--bg-1,#181b1c)}.chat-composer{border:1px solid var(--border,#454b4e);border-radius:7px;overflow:hidden;gap:0}.chat-composer:focus-within{border-color:var(--accent,#e4c956)}.chat-composer textarea{border:0;outline:0;background:transparent;resize:vertical;min-height:66px;max-height:180px;font-size:12px;line-height:1.7}.composer-footer{display:flex;justify-content:space-between;align-items:center;gap:6px;padding:6px 8px;font-size:10px;color:var(--text-2,#c5cbc8);flex-wrap:wrap}.composer-footer .actions button{font-size:10px}.hint,.feedback,.error{margin:0;font-size:10px;line-height:1.6;overflow-wrap:anywhere}.hint{color:var(--text-2,#c5cbc8)}.feedback{color:#77cbb4}.error{color:var(--danger,#ef9292)}.persistent-note{font-size:9px}.checks,.token-list{list-style:none;padding:0;margin:0;font-size:11px}.checks li{display:flex;gap:6px;flex-wrap:wrap;padding:4px 0}.token-list li{display:flex;gap:8px;justify-content:space-between;padding:8px 0;border-bottom:1px solid var(--border,#454b4e)}.token-list li>div{min-width:0;overflow-wrap:anywhere}.token-list button{flex-shrink:0}.identity p{font-size:10px}
@media(max-width:360px){.model-label{max-width:100px}.agent-header{padding:8px}.header-actions{gap:3px}.composer-area,.chat-scroll{padding:10px 8px}.message-user{margin-left:8px}.composer-footer .actions{width:100%;justify-content:flex-end}}
</style>
