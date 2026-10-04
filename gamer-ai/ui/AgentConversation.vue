<script setup>
import { computed, inject, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue'
import { api } from '../../../web/src/api'
import { WORKSPACE_CONTEXT_KEY } from '../../../web/src/workspace/context'
import { budgetValue, displayTime, eventDetails, isActive, stateLabel, tokenUsage, usageValue } from './ai-format'
import { conversationTurns, DELIVERY_LABELS, diagnosticCategory, mergeEvents, safeDiagnostic } from './conversation-format'
import BudgetFields from './BudgetFields.vue'
import MemoryEvidence from './MemoryEvidence.vue'
import AgentMarkdown from './AgentMarkdown.vue'
import RequestContext from './RequestContext.vue'
import {DEFAULT_LIMITS,LIMIT_FIELDS,validLimits} from './budget-format'

const props = defineProps({ packageId: { type: String, default: '' }, attachedMemory: { type: Array, default: () => [] }, active: { type: Boolean, default: true } })
const emit = defineEmits(['detach', 'memory', 'settings'])
const workspace = inject(WORKSPACE_CONTEXT_KEY, null)
const context = computed(() => workspace?.getSnapshot?.() || {})
const deviceId = computed(() => context.value.deviceId || '')
const inputMode = ref('chat'), controlMode = ref('api')
const conversations = ref([]), selectedId = ref(''), conversation = ref(null), events = ref([]), pending = ref([])
const latestSeq = ref(0), oldestSeq = ref(null), moreBefore = ref(false), loadingHistory = ref(false)
const nextConversationCursor = ref(null), catchingUp = ref(false)
const draft = ref(''), error = ref(''), feedback = ref(''), sending = ref(false), busy = ref(false)
const sessions = ref([]), gameId = ref(''), allowResume = ref(false), webSearch = ref(false), searchAvailable = ref(false)
const controlBusy = ref(false)
const boundGame = computed(() => sessions.value.find(session=>session.session_id === (conversation.value?.game_session_id || selectedId.value)))
const detailMode = ref(false), diagnosticsOpen = ref(false), diagnostics = ref([]), diagnosticFilter = ref('all'), diagnosticError = ref(''), exportBusy = ref(false)
const diagnosticOldest=ref(null),moreDiagnostics=ref(false)
const scroll = ref(null), nearBottom = ref(true), saved = ref(null)
const budgetOpen = ref(false), limits = reactive({...DEFAULT_LIMITS})
const gameLimits = reactive({...DEFAULT_LIMITS})
const limitsValid = computed(() => validLimits(startsGame.value || gameSession.value ? gameLimits : limits))
const shownLimits = computed(() => inputMode.value === 'game' || gameSession.value ? gameLimits : limits)
const limitsChanged = computed(() => !conversation.value || LIMIT_FIELDS.some(field => conversation.value.limits?.[field.key] !== limits[field.key]))
const turns = computed(() => conversationTurns(events.value))
const waiting = computed(() => pending.value.filter(message => !events.value.some(event => event.kind === 'user' && event.data?.message_id === message.id)))
const eligibleSessions = computed(() => sessions.value.filter(session => isActive(session) && session.content_package === props.packageId && session.mode === 'api'))
const gameSession = computed(() => eligibleSessions.value.find(session => session.session_id === gameId.value)
  || (inputMode.value === 'game' && isActive(boundGame.value) && boundGame.value.mode === 'api' ? boundGame.value : null))
const activeDeviceSession = computed(() => sessions.value.find(session => session.device_id === deviceId.value && isActive(session)))
const usageLedgers = computed(() => {
  const ledgers = []
  if (conversation.value?.usage) ledgers.push({ key:'chat', label:'对话', usage:conversation.value.usage, limits:conversation.value.limits })
  const game = gameSession.value || (isActive(boundGame.value) ? boundGame.value : null)
  const usage = game?.usage || conversation.value?.game_usage
  if (usage) ledgers.push({ key:'game', label:'游玩', usage, limits:game?.limits || conversation.value?.game_limits })
  return ledgers
})
const mainLedger = computed(() => usageLedgers.value.find(ledger => ledger.key === (inputMode.value === 'game' || gameSession.value ? 'game' : 'chat')) || usageLedgers.value[0])
const startsGame = computed(() => inputMode.value === 'game' && !gameSession.value && !isActive(boundGame.value))
const readonlyHistory = computed(() => ['external','package_deleted'].includes(conversation.value?.state) || boundGame.value?.mode === 'mcp')
const canSend = computed(() => !!props.packageId && (startsGame.value && controlMode.value === 'mcp' || !!saved.value?.has_key)
  && draft.value.trim() && new TextEncoder().encode(draft.value.trim()).length <= 8000 && limitsValid.value
  && !sending.value && !busy.value && !readonlyHistory.value && (!startsGame.value || deviceId.value && !activeDeviceSession.value))
const state = computed(() => conversation.value?.state || 'idle')
const filteredDiagnostics = computed(() => diagnostics.value.filter(event => diagnosticFilter.value === 'all' || diagnosticCategory(event) === diagnosticFilter.value))
let disposed = false, serial = 0, timer, polling = false
const call = (action, values = {}) => api.callExtension('gamer-ai', action, values)
function schedulePoll(delay) {clearTimeout(timer);if(!disposed) timer=setTimeout(poll,delay)}
function applyPage(value, replace = false) {
  if (value.conversation) conversation.value = value.conversation
  events.value = replace ? mergeEvents([], value.events) : mergeEvents(events.value, value.events)
  const received=Math.max(0,...(value.events || []).map(event=>event.seq))
  latestSeq.value = Math.max(latestSeq.value, received)
  catchingUp.value=(value.latest_seq || 0)>latestSeq.value
  if (replace) { oldestSeq.value = value.oldest_seq ?? events.value[0]?.seq ?? null; moreBefore.value = !!value.has_more_before }
  if (replace && value.conversation?.limits) Object.assign(limits,value.conversation.limits)
  if (replace && value.conversation?.game_limits) Object.assign(gameLimits,value.conversation.game_limits)
}
async function list(more = false) {
  if (!props.packageId) { conversations.value = []; return }
  const packageId = props.packageId
  const result = await call('conversation.list', { content_package: packageId, limit: 50,...(more && nextConversationCursor.value ? {cursor:nextConversationCursor.value} : {}) })
  if (!disposed && packageId === props.packageId) {
    conversations.value=more ? [...new Map([...conversations.value,...result.conversations].map(item=>[item.conversation_id,item])).values()] : result.conversations || []
    nextConversationCursor.value=result.next_cursor ?? null
  }
}
async function select(id) {
  const request=++serial; clearTimeout(timer); selectedId.value = id; conversation.value = null; events.value = []; pending.value = []
  diagnosticsOpen.value=false;diagnostics.value=[];diagnosticOldest.value=null;moreDiagnostics.value=false
  latestSeq.value = 0; oldestSeq.value = null; moreBefore.value = false; gameId.value = ''; allowResume.value = false; nearBottom.value = true
  if (id) { try { const value = await call('conversation.get', { conversation_id: id, limit: 80 }); if (!disposed && request === serial) { applyPage(value, true); inputMode.value = value.conversation?.game_session_id ? 'game' : 'chat' } } catch (e) { if (!disposed && request === serial) error.value = e.message || '无法读取对话' } }
  if (!disposed && request===serial) schedulePoll(500)
}
async function newConversation() {
  if (!props.packageId || busy.value) return
  busy.value = true; error.value = ''
  const packageId = props.packageId
  try {
    const result = await call('conversation.create', { content_package: packageId, limits: {...limits} })
    if (disposed || packageId !== props.packageId) return
    await list(); await select(result.conversation.conversation_id)
    feedback.value = '新对话已建立，不需要设备。'
  } catch (e) { if (!disposed) error.value = e.message || '创建对话失败' }
  finally { if (!disposed) busy.value = false }
}
async function poll() {
  if (disposed) return
  clearTimeout(timer)
  if (polling) { schedulePoll(500); return }
  polling = true
  const request = serial, id = selectedId.value
  try {
    const reads = [call('session.get')]
    if (id) reads.push(call('conversation.get', { conversation_id: id, after_seq: latestSeq.value, limit: 80 }))
    const result = await Promise.all(reads)
    if (!disposed && request === serial) {
      sessions.value = result[0].sessions || []
      if (id) applyPage(result[1])
    }
  } catch (e) { if (!disposed && request === serial) error.value = e.message || '状态刷新失败；已接收消息仍由服务端处理' }
  finally { polling = false; schedulePoll(state.value === 'running' || catchingUp.value ? 250 : 1500) }
}
async function history() {
  if (!selectedId.value || !oldestSeq.value || loadingHistory.value) return
  const request = serial, id = selectedId.value, height = scroll.value?.scrollHeight || 0, top = scroll.value?.scrollTop || 0
  loadingHistory.value = true
  try {
    const value = await call('conversation.get', { conversation_id: id, before_seq: oldestSeq.value, limit: 80 })
    if (disposed || request !== serial) return
    events.value = mergeEvents(events.value, value.events); oldestSeq.value = value.oldest_seq ?? oldestSeq.value; moreBefore.value = !!value.has_more_before
    await nextTick(); if (scroll.value) scroll.value.scrollTop = top + scroll.value.scrollHeight - height
  } catch (e) { if (!disposed) error.value = e.message || '历史加载失败' }
  finally { if (!disposed) loadingHistory.value = false }
}
async function send() {
  if (!canSend.value) return
  sending.value = true; error.value = ''; feedback.value = ''
  let receipt
  const message = draft.value.trim(), packageId = props.packageId
  try {
    if (startsGame.value) {
      const result = await call('session.start', { device_id: deviceId.value, content_package: packageId,
        goal: message, mode: controlMode.value, limits: { ...gameLimits } })
      if (disposed || packageId !== props.packageId) return
      draft.value = ''; await list()
      await select(result.conversation_id || result.session?.session_id || result.session_id)
      await poll(); inputMode.value = 'game'
      feedback.value = controlMode.value === 'mcp' ? '外部游玩会话已建立，可在 MCP 设置创建令牌。' : 'AI 游玩已开始，过程与后续指令都保留在本对话。'
      return
    }
    if (!selectedId.value) {
      const result = await call('conversation.create', { content_package: packageId, limits: {...limits} })
      if (disposed || packageId !== props.packageId) return
      await list(); await select(result.conversation.conversation_id)
    }
    const id = selectedId.value, request = serial
    receipt = reactive({ id: `sending:${crypto.randomUUID()}`, text: message, status: 'sending', at: new Date().toISOString() })
    pending.value.push(receipt)
    const result = await call('conversation.message', { conversation_id: id, message,
      attached_memory: props.attachedMemory.filter(item => item.content_package === packageId).map(({ id, revision }) => ({ id, revision })),
      ...(gameSession.value
        ? gameSession.value.limits && LIMIT_FIELDS.some(field=>gameLimits[field.key]!==gameSession.value.limits[field.key]) ? {limits:{...gameLimits}} : {}
        : limitsChanged.value ? {limits:{...limits}} : {}),
      ...(gameSession.value ? { game_session_id: gameSession.value.session_id,
        resume: ['starting','running','resuming'].includes(gameSession.value.state) || allowResume.value } : {}),
      ...(searchAvailable.value ? { web_search: webSearch.value } : {}) })
    if (disposed || request !== serial) return
    Object.assign(receipt, { id: result.message.id, status: result.message.status || 'queued' })
    draft.value = ''; feedback.value = '消息已接收；处理阶段会更新在时间线上。'
    if(result.game?.resume_error) {error.value=`消息已接收，游玩恢复未完成：${typeof result.game.resume_error==='string'?result.game.resume_error:result.game.resume_error.message||'请检查设备暂停原因'}`;feedback.value='新指令已保存；游玩保持当前控制状态。'}
    if (result.conversation) conversation.value = result.conversation
    const value = await call('conversation.get', { conversation_id: id, after_seq: latestSeq.value, limit: 80 })
    if (!disposed && request === serial) applyPage(value)
  } catch (e) {
    if (!disposed) { if (receipt) receipt.status = 'unknown'; error.value = `${e.message || '发送结果未确认'}；请刷新查看接收记录，避免重复提交。` }
  } finally { if (!disposed) sending.value = false }
}
async function withdraw(message) {
  try { await call('conversation.withdraw', { conversation_id: selectedId.value, message_id: message.id }); message.status = 'withdrawn'; await poll() }
  catch (e) { error.value = e.message || '消息已纳入，不能撤回' }
}
async function cancel() {
  try { await call('conversation.cancel', { conversation_id: selectedId.value }); feedback.value = '已请求取消当前问答；上方游玩控制显示设备状态。'; await poll() }
  catch (e) { error.value = e.message || '取消失败' }
}
async function gameControl(action) {
  const session=boundGame.value
  if(!session || controlBusy.value || action==='resume' && !validLimits(gameLimits)) return
  controlBusy.value=true;error.value=''
  try {await call(`session.${action}`,{session_id:session.session_id,
    ...(action==='resume' && session.limits && LIMIT_FIELDS.some(field=>gameLimits[field.key]!==session.limits[field.key]) ? {limits:{...gameLimits}} : {})});await poll()}
  catch(e) {error.value=e.message || '设备控制请求失败'}
  finally {controlBusy.value=false}
}
async function refreshSettings() {
  const [model, services] = await Promise.all([call('settings.get'), call('services.get')])
  if (!disposed) { saved.value = model; searchAvailable.value = !!services.search?.enabled }
}
function changeMode(value) { inputMode.value = value; gameId.value = ''; allowResume.value = false }
function setGameOptions(value) { if(value.limits) Object.assign(gameLimits,value.limits); if(value.mode) controlMode.value=value.mode }
function getGameOptions() { return {limits:{...gameLimits},mode:controlMode.value,session_id:boundGame.value?.session_id || ''} }
watch(() => `${gameSession.value?.session_id}:${gameSession.value?.state}`, () => { if(gameSession.value?.limits) Object.assign(gameLimits,gameSession.value.limits) })
function allReasoning(turn) { return turn.process.filter(item => item.kind === 'thinking' && item.text?.trim()) }
function reasoning(turn) { return allReasoning(turn).slice(-1) }
function earlierReasoning(turn) { return allReasoning(turn).slice(0,-1) }
function tools(turn) { return [...turn.process.filter(item => item.kind !== 'thinking'),
  ...turn.answers.filter(item => item.text).slice(0, -1).map(item => ({ ...item, kind: 'response', label: '过程回复' }))].sort((left, right) => left.seq - right.seq) }
function answers(turn) { return turn.answers.filter(item => item.text).slice(-1) }
function reasoningStatus(turn) {
  if (saved.value?.public_reasoning_content === false) return '公开推理显示已关闭，可在模型设置中开启。'
  return turn.completed || ['idle','paused','finished','interrupted','cancelled','error'].includes(state.value) ? '此轮未记录公开推理或摘要；已保留回答和工具结果。' : '等待模型返回公开推理或摘要…'
}
function diagnosticDetails(event) { return event.kind==='prompt_snapshot' ? JSON.stringify(safeDiagnostic(event).data, null, 2) : eventDetails(event.data) }
async function loadDiagnostics(more = false) {
  diagnosticsOpen.value = true; diagnosticError.value = ''
  if (!selectedId.value) return
  const id=selectedId.value,request=serial
  try {
    const result = await call('conversation.diagnostics', { conversation_id: id, limit: 200,...(more && diagnosticOldest.value ? {before_seq:diagnosticOldest.value} : {}) })
    if(disposed || request!==serial) return
    diagnostics.value=more?mergeEvents(diagnostics.value,result.events):result.events || []
    diagnosticOldest.value=result.oldest_seq ?? result.events?.[0]?.seq ?? null;moreDiagnostics.value=!!result.has_more_before
  }
  catch (e) { diagnosticError.value = e.message || '诊断读取失败' }
}
async function exportDiagnostics() {
  exportBusy.value = true; diagnosticError.value = ''
  const id=selectedId.value,request=serial
  try {
    const result = await call('conversation.diagnostics', { conversation_id: id, export: true })
    if(disposed || request!==serial) return
    const snapshot = JSON.stringify(safeDiagnostic(result), null, 2)
    const url = URL.createObjectURL(new Blob([snapshot], { type: 'application/json' })), anchor = document.createElement('a')
    anchor.href = url; anchor.download = `gamer-ai-diagnostics-${id}.json`; anchor.click(); URL.revokeObjectURL(url)
  } catch (e) { diagnosticError.value = e.message || '诊断导出失败' }
  finally { exportBusy.value = false }
}
function onScroll() { if (scroll.value) nearBottom.value = scroll.value.scrollHeight - scroll.value.scrollTop - scroll.value.clientHeight < 80 }
async function bottom() { nearBottom.value = true; await nextTick(); if (scroll.value) scroll.value.scrollTop = scroll.value.scrollHeight }
function locate(event) {
  const turnId = event.data?.turn_id
  diagnosticsOpen.value = false
  nextTick(() => document.getElementById(`agent-turn-${turnId}`)?.scrollIntoView?.({ block: 'center' }))
}
watch(() => events.value.length + events.value.map(event => event.data?.delta?.length || 0).reduce((a,b) => a+b,0), async () => { if (nearBottom.value && !loadingHistory.value) await bottom() })
watch(() => props.packageId, async () => { await select(''); try { await list(); if (conversations.value.length) await select(conversations.value[0].conversation_id) } catch (e) { if (!disposed) error.value = e.message || '对话列表读取失败' } })
watch(() => props.active, async active => {
  if (!active) return
  try { await refreshSettings() }
  catch(e) { if(!disposed) error.value=e.message || '配置刷新失败' }
})
onMounted(async () => {
  const result = await Promise.allSettled([call('settings.get'), call('services.get'), list(), call('session.get')])
  if (disposed) return
  if (result[0].status === 'fulfilled') saved.value = result[0].value
  if (result[1].status === 'fulfilled') searchAvailable.value = !!result[1].value.search?.enabled
  if (result[3].status === 'fulfilled') sessions.value = result[3].value.sessions || []
  if (conversations.value.length) await select(conversations.value[0].conversation_id)
  else schedulePoll(1000)
  const failed = result.find(item => item.status === 'rejected'); if (failed) error.value = failed.reason?.message || '读取聊天配置失败'
})
onBeforeUnmount(() => { disposed = true; ++serial; clearTimeout(timer) })
defineExpose({select,refreshList:list,refreshSettings,setGameOptions,getGameOptions})
</script>

<template>
  <section class="agent-conversation">
    <header class="agent-header"><div class="agent-identity"><span class="agent-mark" aria-hidden="true">✦</span><div><h3>Agent</h3><button class="model-button" @click="emit('settings','settings')">{{ saved?.model || '配置模型' }} <span aria-hidden="true">⌄</span></button></div></div><div class="actions"><button :disabled="!packageId || busy || !limitsValid" @click="newConversation">新对话</button><button :aria-expanded="budgetOpen" @click="budgetOpen=!budgetOpen">预算</button><button @click="emit('settings','prompts')">提示词</button><button :disabled="!selectedId" @click="loadDiagnostics()">诊断</button><button @click="emit('settings','settings')">设置</button></div></header>
    <section v-if="budgetOpen" class="chat-budget"><p>五项均可设为 0 表示无限。累计用量保留，修改随下一条消息在安全边界生效。</p><BudgetFields :model-value="shownLimits" :prefix="inputMode==='game' || gameSession ? '游玩' : '聊天'" @update:model-value="Object.assign(shownLimits,$event)" /><p v-if="!limitsValid" class="error">非零上限须处于各项允许范围，不能输入负数或小数。</p></section>
    <div class="conversation-bar"><select :value="selectedId" aria-label="聊天历史" @change="select($event.target.value)"><option value="">新的对话</option><option v-for="item in conversations" :key="item.conversation_id" :value="item.conversation_id">{{ item.title || '对话' }} · {{ displayTime(item.updated_at) }}</option></select><button v-if="nextConversationCursor" @click="list(true)">更多对话</button><span class="conversation-state" role="status"><i v-if="state==='running'" class="status-dot" />{{ ({ idle:'等待消息',queued:'已排队',running:boundGame ? '正在游玩' : '正在回答',starting:'正在准备',paused:'游玩已暂停',pausing:'正在暂停',resuming:'正在恢复',stopping:'正在停止',finished:'游玩已结束',interrupted:'已中断',cancelling:'正在取消',cancelled:'已中断',error:'遇到错误',external:'外部 MCP · 只读',package_deleted:'配置包已删除 · 历史' })[state] || state }}</span><button v-if="['running','queued'].includes(state) && (!boundGame || inputMode==='chat')" @click="cancel">取消本轮问答</button></div>
    <div v-if="boundGame" class="game-controls"><div class="game-control-heading"><span class="scope-tag">游玩</span><span>{{ boundGame.device_id }} · {{ stateLabel(boundGame.state) }}</span><button @click="emit('settings','budget')">游玩设置</button></div><div class="actions"><button v-if="['starting','running','resuming'].includes(boundGame.state)" :disabled="controlBusy" @click="gameControl('pause')">暂停 AI 游玩</button><button v-if="boundGame.state==='paused'" :disabled="controlBusy" @click="gameControl('resume')">明确继续 AI 游玩</button><button v-if="isActive(boundGame)" :disabled="controlBusy" @click="gameControl('stop')">停止设备会话</button></div><p v-if="boundGame.state==='paused'">游玩暂停，可人工操作；普通聊天与记忆处理不会恢复设备。</p><p v-else-if="['pausing','stopping'].includes(boundGame.state)">设备操作正在收尾，人工仍锁定，等待已暂停或已结束。</p><p v-else-if="isActive(boundGame)">AI 持有设备控制；需要人工输入时先暂停并等待完成。</p><p v-if="boundGame.pause_reason" class="pause-reason">{{ boundGame.pause_reason.title }}：{{ boundGame.pause_reason.detail }} {{ boundGame.pause_reason.suggestion }}</p></div>
    <div ref="scroll" class="chat-scroll" @scroll="onScroll">
      <div class="transcript">
        <button v-if="moreBefore" class="history-button" :disabled="loadingHistory" @click="history">{{ loadingHistory ? '正在加载…' : '加载更早记录' }}</button>
        <div v-if="!turns.length && !waiting.length" class="empty"><span class="empty-mark" aria-hidden="true">✦</span><h4>今天想完成什么？</h4><p>讨论攻略、整理记忆，或选择游玩让 AI 观察画面并操作。之后可以在同一对话继续引导。</p><div class="empty-actions"><button @click="emit('memory')">查看记忆库</button><button @click="changeMode('game')">开始游玩</button><button @click="emit('settings','settings')">模型设置</button></div><small>{{ packageId ? `配置包 ${packageId}` : '先选择配置包' }} · {{ deviceId || '聊天无需设备' }}</small></div>
        <article v-for="turn in turns" :id="`agent-turn-${turn.id}`" :key="turn.id" class="turn">
          <span v-for="anchor in turn.anchors" :id="`agent-turn-${anchor}`" :key="anchor" class="turn-anchor" aria-hidden="true" />
          <RequestContext v-if="turn.contexts.length" :contexts="turn.contexts" :expanded="turn===turns.at(-1)" @edit="emit('settings','prompts')" />
          <p v-else-if="turn.answers.length || turn.process.length" class="missing-context">此历史轮次未保存模型请求上下文，无法还原当时的前置提示词。</p>
          <div v-for="message in turn.users" :key="message.id" class="user-message"><p>{{ message.text }}</p><div class="message-meta"><span>{{ DELIVERY_LABELS[message.status] || message.status }}</span><time>{{ displayTime(message.at) }}</time><button v-if="message.status === 'queued'" @click="withdraw(message)">撤回</button></div></div>
          <div class="assistant-body">
            <div v-if="reasoning(turn).length || turn.process.length || turn.answers.length" class="assistant-label"><span class="agent-mark" aria-hidden="true">✦</span><b>Agent</b></div>
            <details v-for="item in reasoning(turn)" :key="item.id" class="reasoning" open><summary><span class="think-icon" aria-hidden="true">◌</span>{{ item.label === '公开摘要' ? '推理摘要' : '推理过程' }}<small>{{ item.status === 'streaming' ? '正在思考' : item.status === 'interrupted' ? '已中断' : '已返回' }}</small></summary><div class="reasoning-body"><AgentMarkdown :text="item.text" /></div></details>
            <details v-if="earlierReasoning(turn).length" class="reasoning-history"><summary>此前的推理 · {{ earlierReasoning(turn).length }} 轮</summary><div v-for="item in earlierReasoning(turn)" :key="item.id" class="reasoning-body"><small>{{ item.label }} · {{ displayTime(item.at) }}</small><AgentMarkdown :text="item.text" /></div></details>
            <p v-if="!reasoning(turn).length && (turn.answers.length || turn.process.length || state==='running' && !turn.completed)" class="reasoning-status"><span aria-hidden="true">◌</span>{{ reasoningStatus(turn) }}<button v-if="saved?.public_reasoning_content===false" @click="emit('settings','settings')">开启</button></p>
            <details v-if="tools(turn).length" class="process" :open="detailMode || !turn.completed || tools(turn).some(item => item.status === 'failed')"><summary><span aria-hidden="true">⌁</span>{{ turn.completed ? '已完成的步骤' : '执行步骤' }}<span class="process-count">{{ tools(turn).length }}</span><span class="process-summary">{{ tools(turn).at(-1)?.label }}</span></summary>
              <div v-for="item in tools(turn)" :key="item.id" :class="['process-item',item.kind,item.status]">
                <details v-if="item.kind==='response'" class="process-response"><summary>过程回复<span>{{ item.text.split('\n')[0].slice(0,80) }}</span></summary><AgentMarkdown :text="item.text" /></details>
                <details v-else class="tool-card" :open="item.status==='failed'"><summary><span class="tool-icon" :class="item.status" aria-hidden="true">{{ item.status==='running' ? '◌' : item.status==='failed' ? '!' : '✓' }}</span><b>{{ item.label }}</b><small>{{ ({running:'执行中',complete:'完成',failed:'失败'})[item.status] || item.status }}</small></summary><div class="tool-body"><p v-if="item.message">{{ item.message }}</p>
                  <ul v-if="item.name?.startsWith('memory_') && item.receipt?.items" class="memory-hits"><li v-for="hit in item.receipt.items" :key="hit.id || hit.revision"><b>{{ hit.title || hit.id }}</b> · r{{ hit.revision }}<MemoryEvidence :memory="hit" /><small>{{ hit.summary || hit.reason }}<span v-if="hit.sources?.length"> · {{ hit.sources.length }} 个来源</span></small></li></ul>
                  <p v-if="item.name?.startsWith('memory_') && item.receipt?.saved">{{ item.receipt.receipt_pending ? '正文已落盘，提交收据仍待修复' : '记忆提交已保存' }} · {{ item.receipt.id }}<span v-if="item.receipt.revision"> · r{{ item.receipt.revision }}</span></p>
                  <template v-if="item.name?.startsWith('memory_') && item.receipt?.memory"><p>{{ item.receipt.memory.title }} · r{{ item.receipt.memory.revision }} · {{ item.receipt.memory.applicability || '按当前环境判断适用条件' }}</p><MemoryEvidence :memory="{...item.receipt.memory,source_conflicts:item.receipt.source_conflicts ?? item.receipt.memory.source_conflicts}" show-canonical /></template>
                  <details class="raw-receipt"><summary>参数与实际结果<span v-if="item.stepId"> · {{ item.stepId }}</span></summary><pre>{{ eventDetails(item.data) }}</pre><figure v-if="item.image"><img :src="item.image" alt="该步骤的历史观察截图" /><figcaption>历史观察，当前游戏画面可能已变化。</figcaption></figure></details></div></details>
              </div>
            </details>
            <div v-for="answer in answers(turn)" :key="answer.id" class="final-answer"><AgentMarkdown :text="answer.text" /><div class="answer-meta"><small>{{ answer.status === 'streaming' ? '正在输出…' : answer.status === 'interrupted' ? '输出已中断' : '本轮答复' }}</small><button v-if="answer.status!=='streaming'" @click="loadDiagnostics()">查看过程诊断</button></div></div>
            <div v-for="notice in turn.notices" :key="notice.id" :class="['notice', { 'memory-notice': notice.memory, failed: notice.data?.state==='failed' || notice.data?.state==='error' }]"><div class="notice-heading"><span aria-hidden="true">{{ notice.memory ? '▧' : '!' }}</span><p>{{ notice.text }}</p><button v-if="notice.memory" @click="emit('memory')">查看记忆</button><button v-if="notice.data?.state === 'error' || notice.data?.state === 'failed'" @click="loadDiagnostics()">查看诊断</button></div><small v-if="notice.data?.memory || notice.data?.memory_id">{{ notice.data.memory?.id || notice.data.memory_id }}<span v-if="notice.data.memory?.revision"> · r{{ notice.data.memory.revision }}</span> · {{ notice.data.validation==='pending' || notice.data.memory?.validation==='pending' ? '待验证' : '' }}</small><details v-if="notice.memory && notice.data?.result"><summary>整理进度与结果</summary><pre>{{ eventDetails(notice.data.result) }}</pre></details></div>
          </div>
        </article>
        <div v-for="message in waiting" :key="message.id" class="user-message pending"><p>{{ message.text }}</p><div class="message-meta"><span>{{ message.status === 'sending' ? '正在发送' : DELIVERY_LABELS[message.status] || message.status }}</span><button v-if="message.status === 'queued'" @click="withdraw(message)">撤回</button></div></div>
      </div>
    </div>
    <button v-if="!nearBottom" class="return-bottom" @click="bottom">↓ 回到最新</button>
    <div class="composer"><p v-if="error" role="alert" class="error">{{ error }}</p><p v-if="feedback" role="status" class="feedback">{{ feedback }}</p><p v-if="!saved?.has_key && !(startsGame && controlMode==='mcp')" class="hint">请在模型设置保存 API 连接。<button @click="emit('settings','settings')">配置模型</button></p><p v-if="!packageId" class="hint">先选择配置包；聊天不需要设备。</p>
      <p class="mode-guidance" v-if="!gameSession && inputMode==='chat'">当前为对话：可查询和修改记忆，未授予设备操作。<button type="button" @click="changeMode('game')">切换游玩</button>后发送目标，AI 才能观察画面并操作。</p>
      <p class="mode-guidance" v-else-if="gameSession?.state==='paused'">设备已暂停。只有明确继续 AI 游玩，才会恢复设备操作。</p>
      <p class="mode-guidance" v-else-if="inputMode==='game' && !gameSession">{{ controlMode==='mcp' ? '将建立外部 MCP 控制会话，需要外部 AI 客户端连接后操作。' : '将建立内置 AI 游玩会话，授权当前设备的观察与操作工具。' }}</p>
      <div v-if="attachedMemory.length" class="attachments"><span v-for="item in attachedMemory" :key="item.id">▧ {{ item.title }} · r{{ item.revision }}<button aria-label="移除记忆引用" @click="emit('detach',item.id)">×</button></span></div>
      <p v-if="readonlyHistory" class="hint">此记录仅供回看。需要聊天时请在当前配置包建立新对话；历史输入不会重放。</p>
      <form class="chat-composer" @submit.prevent="send"><textarea v-model="draft" rows="3" aria-label="Agent 消息" :placeholder="inputMode==='game' ? boundGame ? '补充游玩指令，或调整下一步…' : '描述游玩目标，AI 将观察画面并操作…' : '继续讨论，或让 AI 查询和整理记忆…'" :disabled="sending || readonlyHistory" @keydown.ctrl.enter.prevent="send" @keydown.meta.enter.prevent="send" />
        <div class="composer-tools"><select :value="inputMode" aria-label="消息模式" @change="changeMode($event.target.value)"><option value="chat">对话</option><option value="game">游玩</option></select><span v-if="inputMode==='game'" class="composer-device">{{ gameSession?.device_id || deviceId || '未选设备' }}</span><label v-if="searchAvailable" class="search-toggle"><input v-model="webSearch" type="checkbox" />联网</label><button v-if="inputMode==='game'" type="button" @click="emit('settings','budget')">游玩设置</button><button type="submit" class="send-button" :disabled="!canSend">{{ sending ? '发送中…' : startsGame ? controlMode==='mcp' ? '建立外部会话' : '开始游玩' : gameSession && gameSession.state==='paused' && allowResume ? '发送并继续' : '发送' }}<span aria-hidden="true"> ↑</span></button></div>
      </form>
      <div v-if="startsGame" class="start-options"><label>控制方式<select v-model="controlMode" aria-label="开始游玩控制方式"><option value="api">内置 AI</option><option value="mcp">外部 MCP</option></select></label><p v-if="activeDeviceSession" class="hint">当前设备已有活动游玩会话，请打开对应对话或先停止。</p><p v-else-if="!deviceId" class="hint">游玩需要先在工作台选择设备。</p></div>
      <div v-if="gameSession?.state==='paused'" class="resume-choice"><label><input v-model="allowResume" type="checkbox" />明确按此指令继续 AI 游玩</label><small v-if="!allowResume">发送会保留暂停状态。</small></div>
      <details v-if="eligibleSessions.length && (!boundGame || inputMode==='chat')" class="message-options"><summary>关联其他游玩会话</summary><label>游玩指令<select v-model="gameId" aria-label="本条消息关联游戏会话"><option value="">仅聊天与记忆</option><option v-for="session in eligibleSessions" :key="session.session_id" :value="session.session_id">{{ session.device_id }} · {{ stateLabel(session.state) }}</option></select></label><label v-if="gameSession?.state==='paused'"><input v-model="allowResume" type="checkbox" />明确按此指令继续 AI 游玩</label></details>
      <div class="composer-status"><span>{{ packageId || '未选配置包' }}</span><details v-if="mainLedger" class="usage"><summary>{{ mainLedger.label }} · Token {{ tokenUsage(mainLedger.usage) }} / {{ budgetValue(mainLedger.limits,'max_tokens') }}</summary><div class="usage-ledgers"><section v-for="ledger in usageLedgers" :key="ledger.key" :data-ledger="ledger.key"><b>{{ ledger.label }}用量</b><p>Token {{ tokenUsage(ledger.usage) }} / {{ budgetValue(ledger.limits,'max_tokens') }}<small v-if="ledger.usage?.has_unknown_tokens"> · 部分请求用量未知</small></p><p>模型 {{ usageValue(ledger.usage,['turns']) }} / {{ budgetValue(ledger.limits,'max_turns') }} 轮 · 工具 {{ usageValue(ledger.usage,['actions']) }} / {{ budgetValue(ledger.limits,'max_actions') }} 次</p><p>活动 {{ usageValue(ledger.usage,['active_seconds']) }} / {{ budgetValue(ledger.limits,'max_seconds') }} 秒 · 连续失败 {{ usageValue(ledger.usage,['consecutive_failures']) }} / {{ budgetValue(ledger.limits,'max_failures') }}</p></section></div></details><small>Ctrl / ⌘ + Enter 发送</small></div>
    </div>
    <aside v-if="diagnosticsOpen" class="diagnostic-drawer" aria-label="对话诊断"><header><h3>只读诊断</h3><button @click="diagnosticsOpen=false">关闭诊断</button></header><div class="actions"><select v-model="diagnosticFilter" aria-label="诊断类别"><option value="all">全部</option><option value="model">模型请求</option><option value="tool">工具</option><option value="memory">记忆</option><option value="state">状态</option><option value="error">错误</option></select><button @click="loadDiagnostics()">刷新诊断</button><button v-if="moreDiagnostics" @click="loadDiagnostics(true)">加载更早诊断</button><button :disabled="exportBusy" @click="exportDiagnostics">{{ exportBusy ? '正在导出…' : '导出脱敏诊断' }}</button></div><p>导出到本机，不调用模型、不上传。缺失用量、耗时或附件显示未知。</p><p v-if="diagnosticError" role="alert" class="error">{{ diagnosticError }}</p><ol><li v-for="event in filteredDiagnostics" :key="event.seq"><header><b>{{ event.kind }}</b><time>{{ displayTime(event.at) }}</time><button v-if="event.data?.turn_id" @click="locate(event)">定位对话</button></header><p>{{ event.message }}</p><pre>{{ diagnosticDetails(event) }}</pre></li></ol></aside>
  </section>
</template>

<style scoped>
.agent-conversation{display:flex;flex-direction:column;min-height:0;min-width:0;flex:1;position:relative;font-size:12px;overflow:hidden}h3,h4,p{margin:0}p{white-space:pre-wrap;line-height:1.8;overflow-wrap:anywhere}small,.hint{color:var(--text-2,#aab5b2);line-height:1.7}button,input,select,textarea{font:inherit;color:inherit;background:var(--bg-1,#181b1c);border:1px solid var(--border,#41484a);border-radius:6px;padding:6px 8px;min-width:0;box-sizing:border-box}button{cursor:pointer}button:hover:not(:disabled){background:var(--bg-2,#282b2d)}button:disabled{opacity:.45;cursor:default}input[type=checkbox]{width:auto;margin:0;accent-color:var(--accent,#e4c956)}.actions{display:flex;align-items:center;gap:5px;flex-wrap:wrap}.actions button{font-size:11px;padding:4px 7px;border-color:transparent;background:transparent}.agent-header{display:flex;justify-content:space-between;align-items:center;gap:10px;padding:12px 14px 8px}.agent-identity{display:flex;gap:8px;align-items:center;min-width:0}.agent-identity h3{font-size:13px;font-weight:650}.agent-mark{color:var(--accent,#e4c956);font-size:20px;line-height:1}.model-button{font-size:10px;color:var(--text-2,#aab5b2);border:0;padding:2px 0;background:transparent;max-width:180px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.conversation-bar{display:flex;gap:6px;align-items:center;padding:4px 14px 9px;border-bottom:1px solid var(--border,#41484a);flex-wrap:wrap}.conversation-bar select{flex:1;min-width:100px;border:0;background:transparent;font-size:11px;padding:3px 0}.conversation-bar button{font-size:10px;border:0}.conversation-state{display:inline-flex;align-items:center;gap:5px;font-size:10px;color:var(--text-2,#aab5b2)}.status-dot{width:5px;height:5px;border-radius:50%;background:var(--accent,#e4c956)}
.game-controls{margin:8px 14px 0;padding:8px 10px;display:grid;gap:5px;border:1px solid var(--border,#41484a);border-radius:8px;background:var(--bg-2,#24282a);font-size:11px}.game-control-heading{display:flex;align-items:center;gap:7px;flex-wrap:wrap}.game-control-heading>button{margin-left:auto;font-size:10px;padding:2px 0;border:0;background:transparent;color:var(--text-2,#aab5b2)}.scope-tag{border-radius:4px;padding:2px 5px;background:var(--bg-1,#181b1c);color:var(--accent,#e4c956);font-size:10px}.game-controls p{font-size:10px;color:var(--text-2,#aab5b2)}.game-controls .pause-reason{color:var(--accent,#e4c956)}
.chat-budget{padding:12px 14px;border-bottom:1px solid var(--border,#41484a);max-height:220px;overflow:auto}.chat-budget>p{font-size:11px;margin-bottom:8px}.chat-scroll{flex:1;overflow:auto;min-height:0;padding:18px 18px 12px;scrollbar-gutter:stable}.transcript{width:100%;max-width:740px;margin:0 auto}.history-button{display:block;margin:0 auto 20px;border:0;font-size:11px;color:var(--text-2,#aab5b2)}.empty{display:grid;gap:14px;margin:32px 0 24px;color:var(--text-2,#aab5b2);align-content:center}.empty-mark{font-size:36px;color:var(--accent,#e4c956);line-height:1}.empty h4{font-size:18px;font-weight:550;color:var(--text-0,#edf0ee)}.empty p{font-size:13px;max-width:390px}.empty-actions{display:flex;gap:7px;flex-wrap:wrap}.empty-actions button{font-size:11px;background:var(--bg-2,#282b2d);border-color:transparent}.empty small{font-size:10px}.turn{position:relative;display:flex;flex-direction:column;gap:17px;margin-bottom:30px}.user-message{align-self:flex-end;width:fit-content;max-width:87%;margin-left:auto;padding:10px 13px;border-radius:13px 13px 3px 13px;background:var(--bg-2,#282b2d);font-size:13px}.user-message p{line-height:1.75}.message-meta{display:flex;align-items:center;justify-content:flex-end;gap:8px;color:var(--text-2,#aab5b2);font-size:9px;margin-top:5px}.message-meta time{opacity:.75}.message-meta button{padding:0;border:0;background:transparent;font-size:10px;color:var(--text-2,#aab5b2)}.turn-anchor{position:absolute;top:0;left:0;width:0;height:0;margin:0}.reasoning-history>summary{font-size:10px;color:var(--text-2,#aab5b2);padding:3px 0}.reasoning-history .reasoning-body+.reasoning-body{margin-top:10px}.assistant-body{display:grid;gap:9px;min-width:0}.assistant-label{display:flex;gap:7px;align-items:center;font-size:11px;margin-bottom:1px}.assistant-label .agent-mark{font-size:17px}
summary{cursor:pointer}.reasoning>summary,.process>summary,.tool-card>summary{display:flex;align-items:center;gap:7px;list-style:none;min-width:0;font-size:11px;padding:5px 0;color:var(--text-2,#aab5b2)}.reasoning>summary::-webkit-details-marker,.process>summary::-webkit-details-marker,.tool-card>summary::-webkit-details-marker{display:none}.reasoning>summary::after,.process>summary::after,.tool-card>summary::after{content:'';width:5px;height:5px;border-right:1px solid currentColor;border-bottom:1px solid currentColor;transform:rotate(-45deg);margin-left:auto;margin-right:3px;flex-shrink:0}.reasoning[open]>summary::after,.process[open]>summary::after,.tool-card[open]>summary::after{transform:rotate(45deg)}.reasoning small{margin-left:auto;font-size:9px}.reasoning>summary::after{margin-left:3px}.reasoning-body{padding:4px 0 6px 18px;border-left:1px solid var(--border,#41484a);margin-left:5px;color:var(--text-2,#aab5b2)}.reasoning-body :deep(.agent-markdown){font-size:12px;line-height:1.75}.reasoning-status{font-size:10px;display:flex;align-items:center;gap:7px;color:var(--text-2,#aab5b2);line-height:1.6}.reasoning-status button{padding:0;border:0;background:transparent;font-size:10px;color:var(--accent,#e4c956)}
.process{min-width:0}.process-count{font-size:9px;min-width:15px;text-align:center;border-radius:4px;background:var(--bg-2,#282b2d);padding:0 3px}.process-summary{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:10px;opacity:.7}.process-item{padding-left:5px;min-width:0}.tool-card{border-left:1px solid var(--border,#41484a);padding-left:12px;margin-left:3px}.tool-card>summary{padding:6px 0;gap:7px;font-size:11px}.tool-card b{font-weight:500;color:var(--text-0,#edf0ee)}.tool-card small{margin-left:auto;font-size:9px}.tool-card>summary::after{margin-left:0}.tool-icon{color:#77cbb4;font-size:10px;flex-shrink:0}.tool-icon.running{color:var(--accent,#e4c956)}.tool-icon.failed,.failed,.error{color:var(--danger,#ef9292)}.tool-body{font-size:11px;padding:5px 0 9px 17px;color:var(--text-2,#aab5b2);display:grid;gap:7px}.raw-receipt>summary{font-size:10px;color:var(--text-2,#aab5b2)}.process-response{padding:4px 0 5px 16px;color:var(--text-2,#aab5b2)}.process-response>summary{display:flex;gap:7px;font-size:11px;line-height:1.7}.process-response>summary span{font-size:10px;opacity:.7;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;max-width:200px}.process-response :deep(.agent-markdown){font-size:12px}.memory-hits{padding-left:16px;margin:0;line-height:1.8}.memory-hits small{display:block}.final-answer{padding:3px 0 0}.answer-meta{display:flex;gap:8px;align-items:center;margin-top:12px;color:var(--text-2,#aab5b2)}.answer-meta small{font-size:9px}.answer-meta button{padding:0;border:0;background:transparent;font-size:9px;color:var(--text-2,#aab5b2);opacity:.7}
.notice{padding:9px 11px;border-radius:7px;border:1px solid var(--border,#41484a);font-size:11px;color:var(--text-2,#aab5b2)}.notice-heading{display:flex;gap:8px;align-items:center;flex-wrap:wrap}.notice-heading p{flex:1;min-width:140px}.notice-heading button{padding:0;border:0;background:transparent;font-size:10px;color:var(--accent,#e4c956)}.notice>small{padding-left:20px;font-size:10px}.notice>details{margin-top:6px}.notice>details summary{font-size:10px}.memory-notice{background:var(--bg-2,#24282a);border-color:transparent}.notice.failed{border-color:var(--danger,#ef9292)}
.composer{padding:10px 14px 9px;border-top:1px solid var(--border,#41484a);display:grid;gap:7px}.composer>p{font-size:11px}.feedback{font-size:10px;color:#77cbb4;line-height:1.6}.chat-composer{border:1px solid var(--border,#41484a);border-radius:10px;background:var(--bg-2,#24282a);overflow:hidden}.chat-composer:focus-within{border-color:var(--accent,#e4c956)}.chat-composer textarea{display:block;resize:vertical;width:100%;border:0;padding:11px 12px;background:transparent;outline:none;min-height:66px;max-height:170px;font-size:13px;line-height:1.75}.composer-tools{display:flex;gap:6px;align-items:center;padding:5px 8px 8px;flex-wrap:wrap}.composer-tools>select{width:auto;font-size:11px;border:0;background:var(--bg-1,#181b1c);padding:4px 7px}.composer-device{font-size:10px;color:var(--text-2,#aab5b2);max-width:100px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.composer-tools>button:not(.send-button){border:0;background:transparent;font-size:10px;padding:3px 4px;color:var(--text-2,#aab5b2)}.composer-tools .send-button{margin-left:auto;background:var(--accent,#e4c956);border-color:transparent;color:var(--bg-1,#181b1c);border-radius:7px;padding:5px 10px;font-size:11px;font-weight:550}.composer-tools .send-button:hover:not(:disabled){filter:brightness(1.08)}.search-toggle,.resume-choice label{display:flex;align-items:center;gap:5px;font-size:10px;color:var(--text-2,#aab5b2)}.resume-choice{display:flex;gap:7px;flex-wrap:wrap}.resume-choice small{font-size:10px}.start-options{display:flex;gap:8px;align-items:center;flex-wrap:wrap;font-size:10px;color:var(--text-2,#aab5b2)}.start-options label{display:flex;gap:5px;align-items:center}.start-options select{font-size:10px;padding:3px 6px}.message-options{font-size:10px;color:var(--text-2,#aab5b2)}.message-options>label{display:flex;gap:6px;align-items:center;margin-top:6px}.message-options select{font-size:10px;padding:3px}.composer-status{display:flex;gap:8px;align-items:center;justify-content:space-between;color:var(--text-2,#aab5b2);font-size:9px;flex-wrap:wrap}.composer-status>small{font-size:9px}.usage{font-size:9px}.usage p{margin-top:3px}.usage-ledgers{display:grid;gap:9px;padding:9px 10px;margin-top:7px;border-radius:6px;background:var(--bg-2,#24282a)}.usage-ledgers section{min-width:0}.usage-ledgers b{font-size:10px;font-weight:550}.usage-ledgers small{font-size:9px}.attachments{display:flex;gap:6px;flex-wrap:wrap}.attachments span{padding:4px 7px;border-radius:5px;background:var(--bg-2,#282b2d);font-size:10px}.attachments button{border:0;padding:0 5px;background:transparent}.hint button{border:0;padding:0 4px;background:transparent;color:var(--accent,#e4c956);font-size:11px}
pre{white-space:pre-wrap;overflow-wrap:anywhere;padding:8px;background:var(--bg-1,#181b1c);font-size:10px;line-height:1.7;max-height:300px;overflow:auto;border-radius:5px}figure{margin:10px 0}figure img{max-width:100%;max-height:250px;object-fit:contain}figcaption{font-size:10px}.return-bottom{position:absolute;right:20px;bottom:190px;border-radius:16px;box-shadow:0 3px 15px #0003;font-size:10px}.diagnostic-drawer{position:absolute;inset:0;background:var(--bg-1,#181b1c);z-index:2;padding:16px;overflow:auto;display:block}.diagnostic-drawer>header{display:flex;align-items:center;justify-content:space-between;margin-bottom:12px}.diagnostic-drawer p{font-size:11px}.diagnostic-drawer ol{list-style:none;padding:0;display:grid;gap:12px}.diagnostic-drawer li{border:1px solid var(--border,#41484a);padding:10px;border-radius:5px}.diagnostic-drawer li>header{display:flex;gap:8px;align-items:center;flex-wrap:wrap;font-size:11px}.diagnostic-drawer .actions{margin-bottom:10px}:focus-visible{outline:2px solid var(--accent,#e4c956);outline-offset:2px}
@media(max-width:440px){.agent-header{padding:10px 11px 6px;gap:7px}.actions{gap:2px}.actions button{font-size:10px;padding:4px 5px}.model-button{max-width:145px}.conversation-bar{padding:4px 11px 8px}.game-controls{margin:7px 11px 0}.chat-scroll{padding:16px 13px 10px}.user-message{max-width:92%;padding:9px 11px}.composer{padding:9px 11px 8px}.composer-status>small{display:none}.empty{margin-top:22px}.process-summary{max-width:120px}.reasoning-body{padding-left:12px}.tool-body{padding-left:12px}.composer-tools{gap:4px}.composer-device{max-width:65px}}
.missing-context,.mode-guidance{font-size:10px;color:var(--text-2,#aab5b2);line-height:1.7}.mode-guidance button{padding:0 4px;border:0;background:transparent;color:var(--accent,#e4c956);font-size:inherit}
</style>
