<script setup>
import { inject, ref, reactive, computed, watch, nextTick, onMounted, onBeforeUnmount } from 'vue'
import { api } from '../../../web/src/api'
import { WORKSPACE_CONTEXT_KEY } from '../../../web/src/workspace/context'
const context = inject(WORKSPACE_CONTEXT_KEY, null)
const snapshot = () => context?.getSnapshot?.() || {}
const tabs = ['目标', '模型与预算', '授权', '攻略与方案', 'MCP']
const tab = ref('目标'), error = ref(''), busy = ref(false), settings = ref(null), sessions = ref([]), selected = ref(null), events = ref([]), approvals = ref([]), rules = ref([]), usage = ref(null), evidence = ref(''), resources = ref([]), credentials = ref([]), issuedToken = ref('')
const goal = ref(''), profileId = ref(''), planName = ref(''), cursor = ref(0), resumeId = ref(''), billingSnapshot = ref(null)
const advancedOpen = ref(false), protocolMode = ref('auto'), discoveredModels = ref([]), profileMessage = ref('')
const draft = ref(''), questions = ref([]), pendingRunId = ref(''), chatLog = ref(null), now = ref(Math.floor(Date.now() / 1000))
const pendingQuestions = computed(() => questions.value.filter(q => q.answer == null && (!q.timed_out || q.kind !== 'knowledge')))
const pendingApprovals = computed(() => approvals.value.filter(a => a.status === 'pending'))
const chatEvents = computed(() => events.value.filter(e => ['thinking','decision','user_message','question','question_timeout','trial_boundary','plan','action','tool_result','tool_error','terminal','memory_candidate','completion_verified'].includes(e.kind)))
const fieldLabels = { input_micros_per_million: '输入价格（微美元/百万 token）', output_micros_per_million: '输出价格（微美元/百万 token）', max_output_tokens: '每次输出 token 上限', timeout_secs: '请求超时（秒）', session_micros: '逻辑会话金额上限（微美元）', daily_micros: '每天金额上限（微美元）', global_micros: '所有设备累计上限（微美元）', request_micros: '每次请求费用预留（微美元）', max_rounds: '会话请求次数上限', max_searches: '搜索请求次数上限', max_trials: '超时后自动试错操作上限', max_active_secs: '会话运行秒数上限', max_tokens: '会话累计 token 上限', concurrency: '同时请求数量上限' }
const defaultProfile = () => ({ id: 'primary', protocol: 'chat', endpoint: '', model: '', key: '', timeout_secs: 60, max_output_tokens: 1200, price_version: '', input_micros_per_million: 0, output_micros_per_million: 0, cached_micros_per_million: null, cache_creation_micros_per_million: null, vision: 'untested', native_search: 'untested', native_search_enabled: false, native_search_reserve_micros: 0 })
const profile = reactive(defaultProfile())
const rule = reactive({ scope: 'operation', limit: 1, hours: 24, noExpiry: true }), identity = reactive({ account: '', cycle: '' })
const credentialForm = reactive({ hours: 24, tools: 'gamer.goal.submit,gamer.goal.status,gamer.goal.cancel,gamer.session.open,gamer.session.close,observe,act,wait,finish,memory.search,memory.read,memory.propose,search_guides,read_guide,request_approval,ask_user,set_plan,list_automations,call_automation' })
let timer, disposed = false
const call = (action, values = {}) => api.callExtension('gamer-ai', action, values)
async function perform(fn) { if (busy.value) return; busy.value = true; error.value = ''; try { return await fn() } catch (e) { error.value = e.message } finally { busy.value = false } }
async function refresh() {
  const result = await call('sessions.read'); if (disposed) return; sessions.value = result.sessions
  now.value = Math.floor(Date.now() / 1000)
  if (pendingRunId.value) { const s = sessions.value.find(s => s.runs?.includes(pendingRunId.value)); if (s) { selected.value = s; events.value = []; cursor.value = 0; pendingRunId.value = '' } }
  if (selected.value) {
    const [data, pending, costs] = await Promise.all([call('sessions.events', { session_id: selected.value.id, after: cursor.value }), call('approvals.read', { session_id: selected.value.id }), call('usage.read', { session_id: selected.value.id })])
    if (disposed) return
    events.value.push(...data.events); questions.value = data.questions || []; cursor.value = events.value.at(-1)?.seq || cursor.value; selected.value = sessions.value.find(s => s.id === selected.value.id) || selected.value; approvals.value = pending.approvals; rules.value = pending.rules; usage.value = costs
  }
}
async function select(s) { selected.value = s; goal.value = s.goal; draft.value = ''; events.value = []; questions.value = []; cursor.value = 0; evidence.value = ''; pendingRunId.value = ''; await refresh() }
function newChat() { selected.value = null; goal.value = ''; draft.value = ''; events.value = []; questions.value = []; approvals.value = []; usage.value = null; evidence.value = ''; pendingRunId.value = ''; cursor.value = 0 }
async function start() {
  const ctx = snapshot(); if (!ctx.deviceId || !ctx.currentPackageId) throw Error('请先选择设备、Android 应用和配置包')
  const result = await api.run({ runner_id: 'gamer-ai', entrypoint: `${ctx.currentPackageId}#goal`, device_id: ctx.deviceId, content_package: ctx.currentPackageId, payload: { goal: goal.value, model_profile_id: profileId.value, resume_session_id: resumeId.value || null } })
  pendingRunId.value = result.run_id
  resumeId.value = ''; await refresh()
}
async function resume(s) { const ctx = snapshot(); await call('sessions.resume', { session_id: s.id, device_id: ctx.deviceId, model_profile_id: profileId.value }); await refresh() }
async function cancel(s) { await call('sessions.cancel', { session_id: s.id }); await refresh() }
async function takeover() { await call('input.takeover', { device_id: snapshot().deviceId }); await refresh() }
async function sendChat(answer, questionId) {
  const message = (typeof answer === 'string' ? answer : draft.value).trim()
  if (!message) throw Error('请输入目标、补充要求或问题回答')
  if (!selected.value || selected.value.state === 'completed') { goal.value = message; await start(); draft.value = ''; return }
  const question = questionId || pendingQuestions.value[0]?.id
  await call('sessions.message', { session_id: selected.value.id, message, ...(question ? { question_id: question } : {}) })
  draft.value = ''; await refresh()
  if (selected.value.state !== 'running' && !pendingQuestions.value.length && !pendingApprovals.value.length) await resume(selected.value)
}
function onChatEnter(event) { if (event.isComposing) return; event.preventDefault(); perform(sendChat) }
function chatText(event) {
  const data = event.data || {}
  if (event.kind === 'user_message') return data.message
  if (event.kind === 'action') return `执行 ${data.action?.kind || '操作'}：${data.expected || ''}`
  if (event.kind === 'tool_result') return data.result?.blocked ? '这一步需要授权，继续处理其他允许的部分。' : data.result?.status === 'injected' ? '操作已注入，接下来用新画面核对结果。' : `工具 ${data.tool} 已返回结果。`
  if (event.kind === 'memory_candidate') return `已保存攻略候选 ${data.path}，实际验证后才能获得本机信任。`
  return data.summary || data.error || (data.state ? stateLabel[data.state] || data.state : '')
}
watch(() => events.value.length, async () => { const follow = !chatLog.value || chatLog.value.scrollHeight - chatLog.value.scrollTop - chatLog.value.clientHeight < 80; await nextTick(); if (follow && chatLog.value) chatLog.value.scrollTop = chatLog.value.scrollHeight })
function editProfile(p) { Object.assign(profile, defaultProfile(), p, { key: '' }); delete profile.has_key; protocolMode.value = p.protocol; discoveredModels.value = []; profileMessage.value = ''; advancedOpen.value = false }
async function discoverProfile() {
  try {
    const result = await call('settings.discover', { endpoint: profile.endpoint, key: profile.key, protocol: protocolMode.value, profile_id: profile.id })
    discoveredModels.value = result.models
    profile.endpoint = result.endpoint; profile.protocol = result.protocol
    if (!result.models.some(p => p.id === profile.model)) profile.model = result.models[0].id
    profileMessage.value = `已读取模型：${profile.model}。图片与工具能力需单独测试。`
  } catch (e) { advancedOpen.value = true; throw e }
}
async function saveProfile() {
  if (protocolMode.value === 'auto' || !profile.model.trim()) await discoverProfile()
  else profile.protocol = protocolMode.value
  const next = JSON.parse(JSON.stringify(settings.value)); delete next.search?.has_key; if (next.billing) { delete next.billing.has_key; next.billing.key ||= '' }
  next.profiles = next.profiles.filter(p => p.id !== profile.id).map(p => { delete p.has_key; return { ...p, key: '' } }); next.profiles.push({ ...profile })
  settings.value = await call('settings.save', { expected_version: settings.value.version, settings: next }); profile.key = ''
  profileMessage.value = `模型 ${profile.model} 已保存。可点击「测试图片与工具往返」验证，API 测试会产生请求费用。`
}
async function saveSettings() { const next = JSON.parse(JSON.stringify(settings.value)); next.profiles.forEach(p => { delete p.has_key; p.key = '' }); for (const service of [next.search, next.billing]) if (service) { delete service.has_key; service.key ||= '' } settings.value = await call('settings.save', { expected_version: settings.value.version, settings: next }) }
async function testProfile(p, nativeSearch = false) { const r = await call('settings.test', { profile_id: p.id, native_search: nativeSearch }); settings.value = await call('settings.read'); const tested = settings.value.profiles.find(v => v.id === profile.id); if (tested) { profile.vision = tested.vision; profile.native_search = tested.native_search } error.value = `图片与工具往返：${r.vision}；原生搜索：${r.native_search}${r.error ? ` · ${r.error}` : ''}` }
async function syncBilling() { const end = Math.floor(Date.now() / 1000); billingSnapshot.value = await call('usage.sync', { start: end - 86400, end }) }
async function resolve(a, decision, continueRun = false) { await call('approvals.resolve', { approval_id: a.id, decision, scope: rule.scope, limit: Number(rule.limit), ...(rule.scope === 'persistent' && rule.noExpiry ? { no_expiry: true } : { expires_at: Math.floor(Date.now() / 1000) + Number(rule.hours) * 3600 }) }); await refresh(); if (continueRun && selected.value?.state !== 'running' && !pendingQuestions.value.length && !pendingApprovals.value.length) await resume(selected.value) }
async function confirmIdentity() { await call('identity.confirm', { session_id: selected.value.id, account: identity.account, cycle: identity.cycle }); await refresh() }
async function loadResources() { const r = await call('memory.search', { package_id: snapshot().currentPackageId }); resources.value = r.resources }
async function readResource(r) { const result = await call('memory.read', { package_id: snapshot().currentPackageId, path: r.path }); r.content = result.resource.content; r.version = result.resource.version; r.effective_status = result.memory.effective_status }
async function saveResource(r) { await call('memory.update', { package_id: snapshot().currentPackageId, path: r.path, memory: JSON.parse(r.content), expected_version: r.version }); await readResource(r) }
async function savePlan() { await call('plans.save', { package_id: snapshot().currentPackageId, path: `plans/${planName.value}.json`, plan: { goal: goal.value || draft.value, model_profile_id: profileId.value, resume_session_id: null } }); error.value = '方案已保存，可在任务中选择' }
async function viewEvidence(event) { const r = await call('sessions.evidence', { session_id: selected.value.id, observation_id: event.data.observation_id }); evidence.value = `data:image/png;base64,${r.png_b64}` }
async function issueCredential() { const ctx = snapshot(); const result = await call('credentials.issue', { device_id: ctx.deviceId, android_package: ctx.androidPackageName, package_id: ctx.currentPackageId, tools: credentialForm.tools.split(',').map(s => s.trim()).filter(Boolean), expires_at: Math.floor(Date.now() / 1000) + Number(credentialForm.hours) * 3600 }); issuedToken.value = result.token; credentials.value = (await call('credentials.read')).credentials }
async function poll() { try { if (!busy.value) await refresh() } catch (e) { if (!disposed) error.value = e.message } if (!disposed) timer = setTimeout(poll, 2000) }
onMounted(async () => { await perform(async () => { settings.value = await call('settings.read'); await refresh(); credentials.value = (await call('credentials.read')).credentials }); if (!disposed) timer = setTimeout(poll, 2000) })
onBeforeUnmount(() => { disposed = true; clearTimeout(timer); profile.key = ''; issuedToken.value = '' })
const stateLabel = { running: '执行中', waiting_user: '等待你的回答或授权', partial: '部分完成', completed: '完整完成', failed: '失败', cancelled: '已取消' }
const money = value => value == null ? '未知' : (value / 1000000).toFixed(6)
</script>
<template>
  <div class="ai-workspace">
    <nav><button v-for="label in tabs" :key="label" class="btn btn-sm" :class="{ 'btn-primary': tab === label }" @click="tab = label">{{ label }}</button><button class="btn btn-sm" :disabled="busy" @click="perform(refresh)">刷新</button></nav>
    <p v-if="error" class="error" role="alert">{{ error }}</p>
    <template v-if="tab === '目标'">
      <div class="row chat-toolbar"><button class="btn btn-sm" :disabled="busy" @click="newChat">新对话</button><label>多模态模型<select v-model="profileId" class="select" :disabled="selected?.state === 'running'"><option value="">第一个模型</option><option v-for="p in settings?.profiles || []" :key="p.id" :value="p.id">{{ p.model }} · {{ p.vision }}</option></select></label><span v-if="selected" class="hint">{{ stateLabel[selected.state] || selected.state }}</span><button v-if="selected?.state === 'running'" class="btn btn-sm" :disabled="busy" @click="perform(() => cancel(selected))">停止</button><button class="btn btn-sm" :disabled="busy" @click="perform(takeover)">人工接管</button></div>
      <div ref="chatLog" class="chat-log" role="log" aria-label="AI 任务对话" aria-live="polite">
        <p v-if="!selected" class="chat-message assistant">描述你要完成的目标。我会先查阅相关经验和可用攻略，展示计划，再逐步观察、执行和验证。遇到不确定的选择会在这里提问。</p>
        <template v-else>
          <article class="chat-message user"><small>你 · 目标</small><p>{{ selected.goal }}</p></article>
          <article v-for="event in chatEvents" :key="event.seq" class="chat-message" :class="event.kind === 'user_message' ? 'user' : 'assistant'">
            <small>{{ event.kind === 'user_message' ? '你' : event.kind === 'thinking' ? '判断中' : event.kind === 'plan' ? '执行计划' : 'AI' }}</small><p>{{ chatText(event) }}</p>
            <ol v-if="event.kind === 'plan'"><li v-for="(step, i) in event.data.plan.steps" :key="i">{{ step.description }} · 验证：{{ step.expected }}</li></ol>
            <button v-if="event.data.observation_id" class="btn btn-sm" @click="perform(() => viewEvidence(event))">查看这一步画面</button>
            <details><summary>操作详情</summary><pre>{{ event.data }}</pre></details>
          </article>
          <article v-for="q in pendingQuestions" :key="q.id" class="chat-card question-card"><strong>{{ q.question }}</strong><p>{{ q.reason }}</p><small>{{ q.timed_out ? '等待已超时，仍需你决定' : '等待剩余 ' + Math.max(0, q.deadline - now) + ' 秒' }} · {{ q.kind === 'knowledge' ? '超时后仅有限探索' : '超时不会代替你选择' }}</small><div class="row"><button v-for="option in q.options" :key="option" class="btn btn-sm" :disabled="busy || selected.state === 'running'" @click="perform(() => sendChat(option, q.id))">{{ option }}</button></div><p class="hint">也可以在下方输入回答。密码和验证码请用人工接管输入。</p></article>
          <article v-for="a in pendingApprovals" :key="a.id" class="chat-card approval-card"><strong>需要你的授权：{{ a.consumption.resource }} × {{ a.consumption.quantity }}</strong><p>{{ a.consumption.purpose }} · {{ a.consumption.evidence }}</p><button class="btn btn-sm" @click="perform(() => viewEvidence({ data: { observation_id: a.observation } }))">查看消耗画面</button>
            <div class="grid"><label>授权范围<select v-model="rule.scope" class="select"><option value="operation">仅这次操作</option><option value="session">当前目标</option><option value="cycle">已确认游戏周期</option><option value="persistent">永久授权（直到撤销或额度耗尽）</option></select></label><label>总额度<input v-model.number="rule.limit" class="input" type="number" min="1" /></label><label v-if="rule.scope !== 'persistent' || !rule.noExpiry">有效小时<input v-model.number="rule.hours" class="input" type="number" min="1" /></label><label v-if="rule.scope === 'persistent'"><input v-model="rule.noExpiry" type="checkbox" />不设到期时间</label></div>
            <form v-if="['persistent','cycle'].includes(rule.scope) && (!selected.account || (rule.scope === 'cycle' && !selected.cycle))" @submit.prevent="perform(confirmIdentity)"><label>已核实账号<input v-model="identity.account" class="input" required /></label><label v-if="rule.scope === 'cycle'">已核实游戏周期<input v-model="identity.cycle" class="input" required /></label><button type="submit" class="btn btn-sm">确认当前账号范围</button></form>
            <p class="hint">范围绑定当前游戏、账号、资源和用途；聊天文字不会自动批准。AI 先完成其他允许部分，结束运行后才可确认。</p><div class="row"><button class="btn btn-primary btn-sm" :disabled="busy || selected.state === 'running'" @click="perform(() => resolve(a, 'approve', true))">确认授权并继续</button><button class="btn btn-sm" :disabled="busy" @click="perform(() => resolve(a, 'deny'))">拒绝</button></div>
          </article>
        </template>
      </div>
      <form class="chat-composer" @submit.prevent="perform(sendChat)"><label>{{ !selected || selected.state === 'completed' ? '自然语言目标' : pendingQuestions.length ? '回答问题' : '补充要求或活动步骤' }}<textarea v-model="draft" class="input" required rows="3" maxlength="1000" :placeholder="selected ? '补充步骤、回答问题，或说明需要怎样调整；Enter 发送，Shift+Enter 换行' : '例如：完成当前活动，遇到不确定的步骤先问我'" @keydown.enter.exact="onChatEnter" /></label><button class="btn btn-primary" :disabled="busy || !!pendingRunId || (pendingQuestions.length > 0 && selected?.state === 'running')">{{ !selected || selected.state === 'completed' ? '发送目标' : pendingQuestions.length ? '回答并继续' : selected.state === 'running' ? '补充要求' : pendingApprovals.length ? '发送说明' : '发送并继续' }}</button></form>
      <form v-if="selected?.account_reference_only && selected.state !== 'running'" class="identity-reference" @submit.prevent="perform(confirmIdentity)"><p>历史账号 {{ selected.account }} 仅供参考；当前画面无法确认时，可在核实后填写当前账号。账号变化不会继承旧授权或清零用量。</p><label>已核实当前账号<input v-model="identity.account" class="input" required :placeholder="selected.account" /></label><button class="btn btn-sm">确认当前账号</button></form>
      <p class="hint">展示简短判断、计划、动作和结果。补充要求会使尚未执行的旧模型决策失效；立即停止请用停止或接管。攻略长期保存，授权独立确认。</p>
      <section v-if="selected"><details><summary>子目标进度与预算</summary><pre>{{ selected.progress }}</pre><p v-if="usage">估算 {{ money(usage.estimated_micros) }} · 已确认 {{ money(usage.confirmed_micros) }} · 待核对预留 {{ money(usage.pending_micros) }} · 请求 {{ usage.requests.length }}</p><button v-if="selected.state !== 'running'" class="btn btn-sm" @click="perform(async () => { await call('sessions.budget', { session_id: selected.id }); await refresh() })">应用当前预算上限（保留累计用量）</button></details><button v-if="['partial','waiting_user','cancelled'].includes(selected.state)" class="btn btn-sm" :disabled="busy || pendingQuestions.length > 0" @click="perform(() => resume(selected))">重新观察并恢复</button><img v-if="evidence" :src="evidence" alt="当前会话截图证据" class="evidence" /><details v-if="usage"><summary>请求账本与对账</summary><div v-for="r in usage.requests" :key="r.id" class="record"><small>{{ r.kind }} · {{ r.model }} · {{ r.status }} · {{ r.source }} · {{ money(r.actual) }}（预留 {{ money(r.reserved) }}）</small><button v-if="r.actual == null || r.source?.startsWith('estimated')" class="btn btn-sm" @click="perform(async () => { const reference = prompt('供应商账单请求引用'); const amount = prompt('确认费用（微货币单位）'); if (reference && amount !== null) { await call('usage.reconcile', { request_id: r.id, billing_reference: reference, billing_scope: r.profile, confirmed_micros: Number(amount) }); await refresh() } })">录入供应商确认金额</button></div></details></section>
      <details class="chat-history"><summary>历史对话（{{ sessions.length }}）</summary><div v-for="s in sessions" :key="s.id" class="record"><button class="btn btn-sm" @click="perform(() => select(s))">{{ s.goal }} · {{ stateLabel[s.state] || s.state }}</button><button v-if="s.state === 'running' && selected?.id !== s.id" class="btn btn-sm" @click="perform(() => cancel(s))">停止</button><small>{{ s.app }} · {{ s.package }} · {{ s.id }}</small></div></details>
    </template>
    <template v-if="tab === '模型与预算' && settings">
      <p class="hint">先填写服务 URL 和密钥，保存时自动读取模型，不发起推理。本地服务无需密钥。</p>
      <div v-for="p in settings.profiles" :key="p.id" class="record"><strong>{{ p.model }}</strong><span> · 图片 {{ p.vision }} · {{ p.has_key ? '已配置密钥' : '无密钥' }}</span><div class="row"><button class="btn btn-sm" @click="editProfile(p)">编辑</button><button class="btn btn-sm" :disabled="busy" @click="perform(() => testProfile(p))">测试图片与工具往返</button><button v-if="['responses','claude','gemini'].includes(p.protocol)" class="btn btn-sm" :disabled="busy" @click="perform(() => testProfile(p, true))">测试原生搜索组合</button></div></div>
      <form class="model-form" @submit.prevent="perform(saveProfile)">
        <label>服务 URL<input v-model="profile.endpoint" class="input" required type="url" placeholder="https://服务地址/v1 或 http://127.0.0.1:11434" /></label>
        <label>API 密钥<input v-model="profile.key" class="input" type="password" autocomplete="new-password" placeholder="本地服务可留空；编辑时留空保留同一服务的密钥" /></label>
        <p v-if="profileMessage" class="hint" role="status">{{ profileMessage }}</p>
        <details class="model-advanced" :open="advancedOpen" @toggle="advancedOpen = $event.target.open">
          <summary>高级设置（模型、协议与计价）</summary>
          <label>模型配置 ID<input v-model="profile.id" class="input" /></label>
          <label>协议<select v-model="protocolMode" class="select"><option value="auto">自动识别</option><option v-for="p in ['responses','claude','gemini','chat','ollama']" :key="p">{{ p }}</option></select></label>
          <label>模型名<input v-model="profile.model" class="input" list="ai-model-options" placeholder="自动读取，也可手动填写支持图片的模型" /></label>
          <datalist id="ai-model-options"><option v-for="p in discoveredModels" :key="p.id" :value="p.id" /></datalist>
          <button type="button" class="btn btn-sm" :disabled="busy" @click="perform(discoverProfile)">重新读取模型列表</button>
          <p class="hint">通用 API 默认使用兼容 Chat 协议。列表中明确支持图片的模型优先，否则选择候选模型并等待图片测试；列表不等于能力验证。服务未提供列表时可手动指定协议和模型。</p>
          <label>价格版本（可选）<input v-model="profile.price_version" class="input" placeholder="按实际供应商或代理合同填写" /></label>
          <div class="grid"><label v-for="field in ['input_micros_per_million','output_micros_per_million','max_output_tokens','timeout_secs']" :key="field">{{ fieldLabels[field] || field }}<input v-model.number="profile[field]" type="number" class="input" min="0" /></label></div>
          <div class="grid"><label>缓存读取价格（微美元/百万 token，可留空）<input :value="profile.cached_micros_per_million" class="input" type="number" min="0" @input="profile.cached_micros_per_million = $event.target.value === '' ? null : Number($event.target.value)" /></label><label>缓存写入价格（同单位，可留空）<input :value="profile.cache_creation_micros_per_million" class="input" type="number" min="0" @input="profile.cache_creation_micros_per_million = $event.target.value === '' ? null : Number($event.target.value)" /></label></div>
          <label>原生搜索每次费用预留（微美元）<input v-model.number="profile.native_search_reserve_micros" class="input" type="number" min="0" /></label>
          <label><input v-model="profile.native_search_enabled" type="checkbox" :disabled="profile.native_search !== 'available'" />允许已验证的原生搜索</label>
          <p class="hint">原生搜索需单独验证并预留费用；达到搜索请求上限后继续画面探索。</p>
        </details>
        <p class="hint">已使用默认预算。价格可稍后设置；API 未返回费用且未配置价格时会保留预留、停止并提示核对，未知费用不会记为零。图片测试会产生正常请求费用。</p>
        <button class="btn btn-primary" :disabled="busy">保存模型</button>
      </form>
      <details class="budget-advanced"><summary>高级设置（预算、搜索与通知）</summary>
      <h3>预算与运行限制</h3><form @submit.prevent="perform(saveSettings)"><label>提问等待秒数<input v-model.number="settings.question_timeout_secs" class="input" type="number" min="5" max="3600" /></label><div class="grid"><label v-for="field in Object.keys(settings.budget)" :key="field">{{ fieldLabels[field] || field }}<input v-model.number="settings.budget[field]" class="input" type="number" min="0" /></label></div><label>通知通道 ID<input v-model="settings.notification_channel" class="input" placeholder="空值使用现有默认通道" /></label><label><input v-model="settings.notify_results" type="checkbox" />发送运行终态通知</label><details><summary>外部搜索服务（原生搜索不可用时）</summary><button type="button" class="btn btn-sm" @click="settings.search = settings.search || { endpoint: '', key: '', request_micros: 0 }">配置</button><template v-if="settings.search"><label>API 地址<input v-model="settings.search.endpoint" class="input" /></label><label>密钥<input v-model="settings.search.key" class="input" type="password" /></label><label>每次费用预留<input v-model.number="settings.search.request_micros" class="input" type="number" min="0" /></label><p>POST 接收 query/max_results，返回 results 数组（url/title/content）。</p></template></details><details><summary>供应商对账（可选）</summary><button type="button" class="btn btn-sm" @click="settings.billing ||= { protocol: 'openai', endpoint: 'https://api.openai.com/v1', key: '', scope: '' }">配置独立对账凭据</button><template v-if="settings.billing"><label>供应商<select v-model="settings.billing.protocol" class="select"><option value="openai">OpenAI 项目</option><option value="claude">Claude 工作区</option><option value="openrouter">OpenRouter 专用 key</option></select></label><label>对账服务根地址<input v-model="settings.billing.endpoint" class="input" /></label><label>专用项目、工作区或 key 范围<input v-model="settings.billing.scope" class="input" /></label><label>独立对账凭据<input v-model="settings.billing.key" class="input" type="password" placeholder="留空保留同一范围的已有凭据" /></label></template><p class="hint">普通推理密钥不会自动用于管理接口。供应商总账单单独显示，不与 Gamer 请求账本相加。部分账单可能延迟或需要翻页。</p><button type="button" class="btn btn-sm" :disabled="busy" @click="perform(syncBilling)">查询最近一天</button><pre v-if="billingSnapshot">{{ billingSnapshot }}</pre></details><button class="btn btn-primary" :disabled="busy">保存预算与通知</button></form>
      </details>
    </template>
    <template v-if="tab === '授权'">
<p v-if="!selected">先在目标页选择会话。</p><template v-else><p class="hint">授权绑定逻辑会话，恢复与重启不会清零。结果未知的消费保留占用。持久与周期规则必须先确认账号。</p><form @submit.prevent="perform(confirmIdentity)"><label>已核实的账号标识<input v-model="identity.account" class="input" required /></label><label>已核实的游戏周期<input v-model="identity.cycle" class="input" :required="rule.scope === 'cycle'" placeholder="周期授权时填写，依据游戏倒计时或真实重置规则，不默认本地零点" /></label><button class="btn btn-sm">确认账号与周期</button></form><div class="grid"><label>授权范围<select v-model="rule.scope" class="select"><option value="operation">仅这个操作</option><option value="session">当前逻辑会话</option><option value="cycle">已确认周期</option><option value="persistent">已确认账号永久授权</option></select></label><label>总额度<input v-model.number="rule.limit" class="input" type="number" min="1" /></label><label v-if="rule.scope !== 'persistent' || !rule.noExpiry">有效小时<input v-model.number="rule.hours" class="input" type="number" min="1" /></label><label v-if="rule.scope === 'persistent'"><input v-model="rule.noExpiry" type="checkbox" />不设到期时间（直到撤销或额度耗尽）</label></div><div v-for="a in approvals" :key="a.id" class="record"><strong>{{ a.consumption.resource }} · {{ a.consumption.quantity }} · {{ a.status }}</strong><p>{{ a.consumption.purpose }} · {{ a.consumption.evidence }}</p><button class="btn btn-sm" @click="perform(() => viewEvidence({ data: { observation_id: a.observation } }))">查看授权画面</button><button v-if="a.status === 'pending'" class="btn btn-sm" @click="perform(() => resolve(a, 'approve'))">批准所选范围与额度</button><button v-if="a.status === 'pending'" class="btn btn-sm" @click="perform(() => resolve(a, 'deny'))">拒绝</button></div><div v-for="r in rules" :key="r.id" class="record">{{ r.resource }} · {{ r.purpose }} · {{ r.scope }} · 额度 {{ r.limit }} · {{ r.expires_at > 8e12 ? '直到撤销' : new Date(r.expires_at * 1000).toLocaleString() }}<button v-if="!r.revoked" class="btn btn-sm" @click="perform(async () => { await call('approvals.revoke', { rule_id: r.id }); await refresh() })">撤销</button></div><img v-if="evidence" :src="evidence" alt="授权依据画面" class="evidence" /></template>
    </template>
    <template v-if="tab === '攻略与方案'"><label>方案名<input v-model="planName" class="input" placeholder="daily" /></label><button class="btn btn-sm" @click="perform(savePlan)">保存目标页内容为方案</button><button class="btn btn-sm" @click="perform(loadResources)">读取当前包攻略</button><p class="hint">导入、复制、覆盖、重建的攻略均按候选读取。本机验证依赖内容哈希、配置包实例和真实证据。账号、密钥、授权和账本保存在包外；分享前检查攻略正文。</p><div v-for="r in resources" :key="r.path" class="record"><strong>{{ r.path }} · {{ r.effective_status || '未读取' }}</strong><button class="btn btn-sm" @click="perform(() => readResource(r))">读取和编辑</button><template v-if="r.content != null"><textarea v-model="r.content" class="input" rows="6" /><button class="btn btn-sm" @click="perform(() => saveResource(r))">保存修订</button></template><button class="btn btn-sm" @click="perform(async () => { await call('memory.delete', { package_id: snapshot().currentPackageId, path: r.path }); await loadResources() })">删除</button></div></template>
    <template v-if="tab === 'MCP'"><p class="hint">MCP 使用独立受限凭据，只允许本次设备、应用、配置包及工具范围。无法访问管理员 REST、修改配置或自行批准。工具会话空闲 60 秒结束；外部模型账单不计入 Gamer 推理账本。</p><label>有效小时<input v-model.number="credentialForm.hours" class="input" type="number" min="1" max="168" /></label><label>工具范围<textarea v-model="credentialForm.tools" class="input" rows="3" /></label><button class="btn btn-sm" :disabled="busy" @click="perform(issueCredential)">为当前上下文签发凭据</button><div v-if="issuedToken"><p>仅显示一次，关闭页面前保存。</p><input class="input mono" :value="issuedToken" readonly /><button class="btn btn-sm" @click="issuedToken = ''">隐藏</button></div><code>POST /mcp/gamer-ai · Authorization: Bearer &lt;独立凭据&gt;</code><div v-for="c in credentials" :key="c.id" class="record">{{ c.device }} · {{ c.app }} · {{ c.package }} · {{ c.revoked ? '已撤销' : '有效' }}<button v-if="!c.revoked" class="btn btn-sm" @click="perform(async () => { await call('credentials.revoke', { credential_id: c.id }); credentials = (await call('credentials.read')).credentials })">撤销并取消关联运行</button></div></template>
  </div>
</template>
<style scoped>
.ai-workspace{padding:12px;height:100%;overflow:auto;font-size:13px}nav,.row{display:flex;gap:6px;flex-wrap:wrap;margin:8px 0}form{display:grid;gap:8px;margin:12px 0}label{display:grid;gap:4px}.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(140px,1fr));gap:8px}.hint,small{color:var(--text-secondary);font-size:12px;line-height:1.7}.record,.event{padding:10px 0;border-bottom:1px solid var(--border)}pre{white-space:pre-wrap;overflow-wrap:anywhere;font-size:12px}.error{color:var(--danger,#e66)}.evidence{max-width:100%;max-height:500px;object-fit:contain}code{display:block;overflow-wrap:anywhere;margin:8px 0}
.chat-toolbar{align-items:center}.chat-toolbar label{max-width:260px}.chat-log{min-height:180px;max-height:52vh;overflow:auto;display:flex;flex-direction:column;gap:10px;padding:10px;border:1px solid var(--border);border-radius:8px}.chat-message{max-width:92%;padding:9px 12px;border-radius:8px;background:var(--bg-tertiary,var(--bg-secondary));overflow-wrap:anywhere}.chat-message p{margin:4px 0;white-space:pre-wrap}.chat-message.user{align-self:flex-end;background:color-mix(in srgb,var(--primary,#627bff) 15%,var(--bg-secondary))}.chat-message.assistant{align-self:flex-start}.chat-card{border:1px solid var(--border);border-radius:8px;padding:12px}.chat-card p{white-space:pre-wrap}.chat-composer textarea{resize:vertical}.chat-history small{display:block}.chat-log details{font-size:12px}.model-advanced summary,.budget-advanced summary,.chat-history summary{cursor:pointer;padding:6px 0}.model-advanced[open]{display:grid;gap:8px}
</style>
