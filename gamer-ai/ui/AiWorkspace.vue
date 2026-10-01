<script setup>
import { computed, inject, onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import { api } from '../../../web/src/api'
import { WORKSPACE_CONTEXT_KEY } from '../../../web/src/workspace/context'
import { PROTOCOLS, displayTime, eventDetails, eventImage, isActive, stateLabel, usageValue } from './ai-format'

const workspace = inject(WORKSPACE_CONTEXT_KEY, null)
const context = computed(() => workspace?.getSnapshot?.() || {})
const deviceId = computed(() => context.value.deviceId || '')
const packageId = computed(() => context.value.currentPackageId || '')
const deviceName = computed(() => context.value.device?.name || deviceId.value || '未选择设备')
const activeTab = ref('play')
const tabs = [{ key: 'play', label: '自动游玩' }, { key: 'settings', label: '模型连接' }, { key: 'mcp', label: '外部 MCP' }]
const saved = ref(null), sessions = ref([]), selectedId = ref(''), tokens = ref([])
const busy = ref(''), controlBusy = ref(''), error = ref(''), feedback = ref(''), statusFresh = ref(false)
const goal = ref(''), mode = ref('api')
const limits = reactive({ max_turns: 40, max_actions: 120, max_seconds: 600, max_tokens: 100000, max_failures: 3 })
const limitFields = [
  { key: 'max_turns', label: '最大模型轮数', min: 1, max: 500 },
  { key: 'max_actions', label: '最大工具次数', min: 1, max: 2000 },
  { key: 'max_seconds', label: '最长活动时长（秒）', min: 10, max: 7200 },
  { key: 'max_tokens', label: '累计 token 上限', min: 2048, max: 2000000 },
  { key: 'max_failures', label: '连续失败上限', min: 1, max: 20 },
]
const settings = reactive({ base_url: PROTOCOLS.responses.baseUrl, model: 'glm-5.3-flash', protocol: 'responses', request_timeout_secs: 60, api_key: '' })
const tokenForm = reactive({ label: '', control: false, ttl_seconds: 120 })
const createdToken = ref(null), copied = ref('')
const mcpEndpoint = `${globalThis.location?.origin || 'http://127.0.0.1:8443'}/api/extensions/gamer-ai/mcp`
const selectedSession = computed(() => sessions.value.find(s => s.session_id === selectedId.value)
  || sessions.value.find(s => s.device_id === deviceId.value && isActive(s))
  || sessions.value.find(s => s.device_id === deviceId.value) || null)
const currentActive = computed(() => sessions.value.find(s => s.device_id === deviceId.value && isActive(s)))
const controlSession = computed(() => sessions.value.find(s => s.device_id === deviceId.value
  && s.content_package === packageId.value && s.mode === 'mcp' && isActive(s)))
const goalBytes = computed(() => new TextEncoder().encode(goal.value.trim()).length)
const canStart = computed(() => !!deviceId.value && !!packageId.value && !!goal.value.trim() && goalBytes.value <= 8000
  && statusFresh.value && !currentActive.value && (mode.value === 'mcp' || !!saved.value?.has_key))
const boundElsewhere = computed(() => selectedSession.value && (selectedSession.value.device_id !== deviceId.value || selectedSession.value.content_package !== packageId.value))
const canPause = computed(() => statusFresh.value && ['starting', 'running', 'resuming'].includes(selectedSession.value?.state))
const canResume = computed(() => statusFresh.value && selectedSession.value?.state === 'paused')
const events = computed(() => (selectedSession.value?.events || []).slice(-200))
const lastImage = computed(() => [...events.value].reverse().map(eventImage).find(Boolean) || '')
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
  if (!canStart.value) return
  await operate('正在开始', async () => {
    const result = await call('session.start', { device_id: deviceId.value, content_package: packageId.value,
      goal: goal.value.trim(), mode: mode.value, limits: { ...limits } })
    selectedId.value = result?.session?.session_id || result?.session_id || ''
    await refresh()
    feedback.value = mode.value === 'mcp' ? '外部控制会话已建立；客户端可使用匹配目标的控制令牌调用工具。' : 'AI 会话已提交。'
  })
}
async function control(action) {
  const session = selectedSession.value
  if (!session || controlBusy.value) return
  controlBusy.value = action === 'pause' ? '正在请求暂停' : action === 'resume' ? '正在请求恢复' : '正在请求停止'
  error.value = ''; feedback.value = ''
  try {
    await call(`session.${action}`, { session_id: session.session_id })
    await refresh()
  } catch (e) { if (!disposed) error.value = e.message || '会话控制失败' }
  finally { if (!disposed) controlBusy.value = '' }
}
async function saveSettings(test = false) {
  await operate(test ? '保存并测试连接' : '保存连接设置', async () => {
    const values = { expected_version: saved.value?.version ?? null, base_url: settings.base_url.trim(),
      model: settings.model.trim(), protocol: settings.protocol, request_timeout_secs: settings.request_timeout_secs }
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
      label: tokenForm.label.trim(), control: tokenForm.control, ttl_seconds: tokenForm.ttl_seconds })
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
    <nav class="workbench-tabs" aria-label="AI 助手功能">
      <button v-for="tab in tabs" :key="tab.key" type="button" class="tab-btn" :class="{ active: activeTab === tab.key }"
        :aria-pressed="activeTab === tab.key" :aria-controls="`ai-${tab.key}`" @click="activeTab = tab.key">{{ tab.label }}</button>
    </nav>
    <div class="ai-content">
      <p v-if="error" role="alert" class="error">{{ error }}</p>
      <p v-if="feedback" role="status" class="feedback">{{ feedback }}</p>
      <p v-if="busy" role="status" class="hint">{{ busy }}…</p>
      <p v-if="controlBusy" role="status" class="hint">{{ controlBusy }}…</p>
      <div v-show="activeTab === 'play'" id="ai-play" class="section-stack">
        <section aria-label="游玩目标">
          <div class="heading"><h3>游玩目标</h3><span class="tag">通用视觉操作</span></div>
          <div class="context-line"><span>设备：{{ deviceName }}</span><span>配置包：{{ packageId || '未选择' }}</span><span v-if="context.androidPackageName">应用：{{ context.androidPackageName }}</span></div>
          <p v-if="!deviceId || !packageId" class="hint">请在工作台选择设备和配置包。应用目标取设备配置，配置包提供数据上下文。</p>
          <form @submit.prevent="start">
            <fieldset :disabled="!!busy || !!currentActive">
              <label>控制方式<select v-model="mode" aria-label="控制方式"><option value="api">内置 AI · 模型 API</option><option value="mcp">外部 AI · MCP 客户端</option></select></label>
              <label>目标描述<textarea v-model="goal" aria-label="目标描述" rows="4" maxlength="8000" required placeholder="描述需要完成的目标、成功条件以及操作限制" /></label>
              <p v-if="goalBytes > 8000" class="error">目标超过 8000 字节，请缩短描述。</p>
              <p class="hint">根据截图判断和操作，不预设游戏流程。首版适合允许等待模型响应的操作。</p>
              <details class="budget-settings" open><summary>运行预算</summary><div class="budget-grid"><label v-for="field in limitFields" :key="field.key">{{ field.label }}<input v-model.number="limits[field.key]" :aria-label="field.label" type="number" :min="field.min" :max="field.max" step="1" required /></label></div></details>
              <button type="submit" class="primary" :disabled="!canStart">{{ mode === 'mcp' ? '建立外部控制会话' : '开始自动游玩' }}</button>
            </fieldset>
          </form>
          <p v-if="mode === 'api' && !saved?.has_key" class="hint">先在“模型连接”中保存 API 密钥并测试连接。</p>
          <p v-if="mode === 'mcp'" class="hint">先建立会话，再在“外部 MCP”创建控制令牌。外部客户端只能控制授权的目标；暂停后客户端不能自行恢复。</p>
          <p v-if="currentActive" class="hint">当前设备已有 AI 会话；暂停时保留运行槽，先停止才能开始另一次运行。</p>
        </section>
        <section aria-label="AI 会话">
          <div class="heading"><h3>当前会话</h3><button type="button" :disabled="!!busy" @click="operate('刷新状态', refresh)">刷新</button></div>
          <label v-if="sessions.length > 1">查看会话<select :value="selectedSession?.session_id || ''" aria-label="查看会话" @change="selectedId = $event.target.value"><option v-for="session in sessions" :key="session.session_id" :value="session.session_id">{{ sessionTitle(session) }}</option></select></label>
          <template v-if="selectedSession">
            <p class="session-state" role="status" data-testid="session-state">{{ stateLabel(selectedSession.state) }}</p>
            <p class="hint control-hint" data-testid="control-hint">{{ controlHint(selectedSession) }}</p>
            <p v-if="!statusFresh" class="error">状态尚未同步，暂停和继续暂不可用；仍可请求停止。</p>
            <p v-if="boundElsewhere" class="hint">此会话绑定设备 {{ selectedSession.device_id }} / 配置包 {{ selectedSession.content_package }}，工作台切换不会更改它的运行目标。</p>
            <p class="session-goal">{{ selectedSession.goal }}</p>
            <div class="actions"><button v-if="canPause" type="button" :disabled="!!controlBusy" @click="control('pause')">暂停 AI</button><button v-if="canResume" type="button" class="primary" :disabled="!!controlBusy" @click="control('resume')">继续 AI</button><button v-if="isActive(selectedSession)" type="button" :disabled="!!controlBusy" @click="control('stop')">停止会话</button></div>
            <p v-if="selectedSession.reason" class="reason">{{ selectedSession.reason }}</p>
            <dl class="usage-grid"><div><dt>模型轮数</dt><dd>{{ usageValue(selectedSession.usage, ['turns']) }} / {{ selectedSession.limits?.max_turns }}</dd></div><div><dt>工具调用</dt><dd>{{ usageValue(selectedSession.usage, ['actions']) }} / {{ selectedSession.limits?.max_actions }}</dd></div><div><dt>活动秒数</dt><dd>{{ usageValue(selectedSession.usage, ['active_seconds']) }} / {{ selectedSession.limits?.max_seconds }}</dd></div><div><dt>累计 token</dt><dd>{{ usageValue(selectedSession.usage, ['total_tokens']) }} / {{ selectedSession.limits?.max_tokens }}</dd></div><div><dt>连续失败</dt><dd>{{ usageValue(selectedSession.usage, ['consecutive_failures']) }} / {{ selectedSession.limits?.max_failures }}</dd></div></dl>
            <p class="hint">暂停不重置预算。达到预算或连续失败上限时暂停；停止后可调整预算并开始新会话。供应商未返回 token 用量时显示“未知”。</p>
            <details class="identity"><summary>会话标识</summary><p>Session：{{ selectedSession.session_id }}</p><p>Run：{{ selectedSession.run_id }}</p><p>控制方式：{{ selectedSession.mode === 'mcp' ? '外部 MCP' : '内置 API' }} · generation {{ selectedSession.generation }}</p></details>
          </template>
          <p v-else class="hint">尚无会话。关闭面板不会停止服务端运行；需要结束时使用“停止会话”。</p>
        </section>
        <section v-if="selectedSession" aria-label="观察与操作记录">
          <div class="heading"><h3>观察与操作记录</h3><span class="hint">最近 {{ events.length }} 条</span></div>
          <figure v-if="lastImage"><img :src="lastImage" alt="AI 最近一次观察的目标截图" /><figcaption>最近一次观察；投屏画面可能已发生变化。</figcaption></figure>
          <ol v-if="events.length" class="events"><li v-for="(event, index) in events" :key="event.seq ?? index"><div class="event-header"><time>{{ displayTime(event.at) }}</time><b>{{ event.kind }}</b><span v-if="eventImage(event)">截图</span></div><p>{{ event.message }}</p><details v-if="eventDetails(event.data)"><summary>详情</summary><pre>{{ eventDetails(event.data) }}</pre></details></li></ol>
          <p v-else class="hint">等待观察或操作事件。</p>
        </section>
      </div>
      <div v-show="activeTab === 'settings'" id="ai-settings" class="section-stack">
        <section aria-label="模型连接设置">
          <h3>模型连接</h3>
          <p class="hint">连接设置保存在运行 Gamer 服务端的电脑，密钥不随配置包导出。协议切换仅在显式保存后生效。</p>
          <form autocomplete="off" @submit.prevent="saveSettings(false)"><fieldset :disabled="!!busy">
            <label>API 协议<select v-model="settings.protocol" aria-label="API 协议"><option value="responses">Responses</option><option value="chat_completions">Chat Completions</option></select></label>
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
      <div v-show="activeTab === 'mcp'" id="ai-mcp" class="section-stack">
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
            <label>控制租约超时（秒）<input v-model.number="tokenForm.ttl_seconds" aria-label="控制租约超时（秒）" type="number" min="30" max="3600" step="1" required /></label>
            <button type="submit" :disabled="!deviceId || !packageId || (tokenForm.control && !controlSession)">创建连接令牌</button>
          </fieldset></form>
          <p v-if="tokenForm.control && !controlSession" class="hint">请先在“自动游玩”选择“外部 AI”并建立当前设备与配置包的控制会话。</p>
          <p v-if="controlSession" class="hint">外部控制会话：{{ stateLabel(controlSession.state) }}。控制客户端仍须遵守暂停状态，不能自行恢复或更换目标。</p>
          <template v-if="createdToken"><label>新连接令牌<input :value="createdToken.token" type="password" readonly aria-label="新连接令牌" @focus="$event.target.select()" /></label><div class="actions"><button type="button" @click="copy(createdToken.token, '令牌')">{{ copied === '令牌' ? '已复制令牌' : '复制令牌' }}</button><button type="button" @click="createdToken = null">收起令牌</button></div><p class="hint">令牌有效期 24 小时；控制租约 {{ createdToken.ttl_seconds }} 秒。客户端须在租约期限内发送有效工具请求或 ping 续租；失联、令牌到期或撤销会暂停，需要用户恢复。</p></template>
          <details :open="!!createdToken"><summary>客户端配置示例</summary><pre>{{ clientConfig }}</pre><button type="button" @click="copy(clientConfig, '配置')">{{ copied === '配置' ? '已复制配置' : '复制配置' }}</button><p class="hint">示例适用于支持 URL 与 headers 的 MCP 客户端，请按客户端要求填写。完整令牌只在创建时提供，不会写入配置包或浏览器存储。</p></details>
        </section>
        <section aria-label="连接令牌列表">
          <div class="heading"><h3>连接令牌</h3><button type="button" :disabled="!!busy" @click="operate('刷新令牌', refreshTokens)">刷新</button></div>
          <ul v-if="tokens.length" class="token-list"><li v-for="token in tokens" :key="token.token_id"><div><b>{{ token.label || token.token_id }}</b><p class="hint">{{ token.device_id }} · {{ token.content_package }} · {{ token.control ? '观察与操作' : '只读观察' }}</p><p class="hint">到期：{{ token.expires_at ? new Date(typeof token.expires_at === 'number' && token.expires_at < 1e12 ? token.expires_at * 1000 : token.expires_at).toLocaleString('zh-CN') : '未返回' }}</p></div><button type="button" :disabled="!!busy" @click="revokeToken(token)">撤销</button></li></ul>
          <p v-else class="hint">暂无连接令牌。</p>
        </section>
      </div>
      <p class="hint persistent-note">关闭面板不会停止 AI。停用插件或退出服务端会结束会话；控制令牌失效会暂停外部会话。</p>
    </div>
  </div>
</template>

<style scoped>
.ai-workspace{display:flex;flex:1;min-height:0;min-width:0;flex-direction:column;gap:8px;overflow:hidden;color:var(--text-0,#edf0ee)}
.workbench-tabs{display:flex;gap:4px;flex-shrink:0;border-bottom:1px solid var(--border,#454b4e);padding-bottom:6px;flex-wrap:wrap}
.tab-btn{height:28px;padding:3px 10px;border:1px solid transparent;border-radius:3px;background:transparent;color:var(--text-2,#c5cbc8);font-size:13px}.tab-btn.active{border-color:var(--border,#454b4e);background:var(--bg-2,#282b2d);color:var(--text-0,#edf0ee);font-weight:700}
.ai-content{flex:1;min-height:0;overflow:auto;display:flex;flex-direction:column;gap:12px;padding:8px;scrollbar-gutter:stable}.ai-content>*{box-sizing:border-box;flex-shrink:0;width:100%;max-width:820px;margin-inline:auto}.section-stack{display:grid;gap:16px}section{display:grid;gap:12px;border:1px solid var(--border,#454b4e);border-radius:6px;padding:16px;min-width:0}
h3,p,figure{margin:0}h3{font-size:15px}.heading{display:flex;align-items:center;justify-content:space-between;gap:12px;flex-wrap:wrap}.hint,figcaption{font-size:12px;color:var(--text-2,#c5cbc8);line-height:1.7}.context-line{display:flex;gap:6px 16px;flex-wrap:wrap;font-size:12px;color:var(--text-2,#c5cbc8);overflow-wrap:anywhere}.tag{font-size:11px;padding:3px 7px;background:var(--bg-2,#282b2d);border-radius:3px;color:var(--text-2,#c5cbc8)}
fieldset{border:0;padding:0;margin:0;min-width:0;display:grid;gap:12px}label{display:grid;gap:6px;font-size:13px}input,select,textarea{box-sizing:border-box;width:100%;min-width:0;padding:8px;border:1px solid var(--border,#454b4e);border-radius:3px;background:var(--bg-1,#181b1c);color:inherit;font:inherit}textarea{resize:vertical;line-height:1.6}button{padding:7px 12px;border-radius:3px;border:1px solid var(--border,#454b4e);background:var(--bg-2,#282b2d);color:inherit;cursor:pointer;justify-self:start;font-size:12px}button:hover:not(:disabled){border-color:var(--accent,#e4c956)}button.primary{color:var(--accent,#e4c956);border-color:var(--accent,#e4c956)}button:disabled,fieldset:disabled{opacity:.55;cursor:default}button:focus-visible,input:focus-visible,select:focus-visible,textarea:focus-visible,summary:focus-visible{outline:2px solid var(--accent,#e4c956);outline-offset:2px}form{display:grid;gap:12px}.actions{display:flex;gap:8px;flex-wrap:wrap}
.budget-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(140px,1fr));gap:10px;padding-top:12px}summary{font-size:12px;cursor:pointer;color:var(--text-2,#c5cbc8)}.session-state{font-size:14px;color:var(--accent,#e4c956);font-weight:700}.session-goal,.reason{line-height:1.7;font-size:13px;white-space:pre-wrap;overflow-wrap:anywhere}.reason{color:var(--text-2,#c5cbc8)}.usage-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(140px,1fr));gap:12px;margin:0}.usage-grid dt{font-size:11px;color:var(--text-2,#c5cbc8)}.usage-grid dd{font-size:12px;margin:4px 0 0}.identity p{font-size:11px;overflow-wrap:anywhere;margin-top:6px;color:var(--text-2,#c5cbc8)}
figure{display:grid;gap:6px}figure img{display:block;max-width:100%;max-height:360px;object-fit:contain;justify-self:center;border:1px solid var(--border,#454b4e)}.events,.checks,.token-list{padding:0;margin:0;list-style:none}.events{max-height:440px;overflow:auto}.events li{display:grid;gap:6px;padding:10px 0;border-bottom:1px solid var(--border,#454b4e);font-size:12px}.events li p{white-space:pre-wrap;line-height:1.6;overflow-wrap:anywhere}.event-header{display:flex;gap:10px;align-items:center;flex-wrap:wrap}.event-header time{color:var(--text-2,#c5cbc8);font-size:11px}.event-header b{color:var(--accent,#e4c956);font-size:11px}.event-header span{font-size:11px}.error{color:var(--danger,#ef9292);font-size:12px;overflow-wrap:anywhere;line-height:1.6}.feedback{color:#77cbb4;font-size:12px;line-height:1.6}.checks li{display:flex;gap:8px;align-items:baseline;flex-wrap:wrap;font-size:12px;padding:6px 0;overflow-wrap:anywhere}.endpoint-tip{display:grid;gap:6px}.endpoint-tip .hint{overflow-wrap:anywhere}pre{font-size:11px;line-height:1.6;white-space:pre-wrap;overflow-wrap:anywhere;background:var(--bg-1,#181b1c);padding:10px;border-radius:3px;max-height:260px;overflow:auto}.token-list li{display:flex;justify-content:space-between;align-items:center;gap:12px;padding:10px 0;border-bottom:1px solid var(--border,#454b4e)}.token-list li>div{min-width:0;overflow-wrap:anywhere;font-size:12px}.token-list button{flex-shrink:0}.persistent-note{text-align:center}
@media(max-width:420px){section{padding:12px}.budget-grid,.usage-grid{grid-template-columns:repeat(2,minmax(0,1fr))}.context-line{display:grid;gap:4px}.actions button{flex:1}.ai-content{padding:4px}}
</style>
