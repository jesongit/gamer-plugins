<script setup>
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue'
import { api } from '../../../web/src/api'
import { budgetValue, displayTime, eventDetails, isActive, stateLabel, tokenUsage, usageValue } from './ai-format'
import { conversationTurns, DELIVERY_LABELS, diagnosticCategory, mergeEvents, safeDiagnostic } from './conversation-format'
import BudgetFields from './BudgetFields.vue'
import MemoryEvidence from './MemoryEvidence.vue'
import {DEFAULT_LIMITS,LIMIT_FIELDS,validLimits} from './budget-format'

const props = defineProps({ packageId: { type: String, default: '' }, attachedMemory: { type: Array, default: () => [] }, active: { type: Boolean, default: true } })
const emit = defineEmits(['detach', 'memory', 'game'])
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
const limitsValid = computed(() => validLimits(limits))
const limitsChanged = computed(() => !conversation.value || LIMIT_FIELDS.some(field => conversation.value.limits?.[field.key] !== limits[field.key]))
const turns = computed(() => conversationTurns(events.value))
const waiting = computed(() => pending.value.filter(message => !events.value.some(event => event.kind === 'user' && event.data?.message_id === message.id)))
const eligibleSessions = computed(() => sessions.value.filter(session => isActive(session) && session.content_package === props.packageId && session.mode === 'api'))
const gameSession = computed(() => eligibleSessions.value.find(session => session.session_id === gameId.value))
const readonlyHistory = computed(() => ['external','package_deleted'].includes(conversation.value?.state))
const canSend = computed(() => !!props.packageId && !!saved.value?.has_key && draft.value.trim() && new TextEncoder().encode(draft.value.trim()).length <= 8000 && limitsValid.value && !sending.value && !busy.value && !readonlyHistory.value)
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
  if (id) { try { const value = await call('conversation.get', { conversation_id: id, limit: 80 }); if (!disposed && request === serial) applyPage(value, true) } catch (e) { if (!disposed && request === serial) error.value = e.message || '无法读取对话' } }
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
      ...(limitsChanged.value ? {limits:{...limits}} : {}),
      ...(gameSession.value ? { game_session_id: gameSession.value.session_id, resume: allowResume.value } : {}),
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
  try { await call('conversation.cancel', { conversation_id: selectedId.value }); feedback.value = '已请求取消当前问答。设备控制状态请查看游玩页。'; await poll() }
  catch (e) { error.value = e.message || '取消失败' }
}
async function gameControl(action) {
  const session=boundGame.value
  if(!session || controlBusy.value) return
  controlBusy.value=true;error.value=''
  try {await call(`session.${action}`,{session_id:session.session_id});await poll()}
  catch(e) {error.value=e.message || '设备控制请求失败'}
  finally {controlBusy.value=false}
}
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
  try { const [model,services] = await Promise.all([call('settings.get'),call('services.get')]); if (!disposed) {saved.value=model;searchAvailable.value=!!services.search?.enabled} }
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
defineExpose({select,refreshList:list})
</script>

<template>
  <section class="agent-conversation">
    <header><div><h3>Agent 对话</h3><small>配置包 {{ packageId || '未选择' }} · {{ saved?.model || '请配置模型' }}</small></div><div class="actions"><button :disabled="!packageId || busy || !limitsValid" @click="newConversation">新对话</button><button @click="budgetOpen=!budgetOpen">聊天预算</button><button :disabled="!selectedId" @click="loadDiagnostics()">诊断</button><label><input v-model="detailMode" type="checkbox" />详细过程</label></div></header>
    <section v-if="budgetOpen" class="chat-budget"><p>五项均可设为 0 表示无限；累计用量保留，修改随下一条消息在安全边界生效。</p><BudgetFields :model-value="limits" prefix="聊天" @update:model-value="Object.assign(limits,$event)" /><p v-if="!limitsValid" class="error">非零上限须处于各项允许范围，不能输入负数或小数。</p></section>
    <div class="conversation-bar"><select :value="selectedId" aria-label="聊天历史" @change="select($event.target.value)"><option value="">新的对话</option><option v-for="item in conversations" :key="item.conversation_id" :value="item.conversation_id">{{ item.title || '对话' }} · {{ displayTime(item.updated_at) }}</option></select><button v-if="nextConversationCursor" @click="list(true)">更多对话</button><span role="status">{{ ({ idle:'等待消息',queued:'已排队',running:'正在回答',cancelling:'正在取消',cancelled:'已中断',error:'遇到错误',external:'外部 MCP 记录（只读）',package_deleted:'配置包已删除（仅历史）' })[state] || state }}</span><button v-if="['running','queued'].includes(state)" @click="cancel">取消本轮问答</button></div>
    <div v-if="boundGame" class="game-controls"><span>{{ boundGame.device_id }} · {{ stateLabel(boundGame.state) }}</span><button v-if="['starting','running','resuming'].includes(boundGame.state)" :disabled="controlBusy" @click="gameControl('pause')">暂停 AI 游玩</button><button v-if="boundGame.state==='paused'" :disabled="controlBusy" @click="gameControl('resume')">明确继续 AI 游玩</button><button v-if="isActive(boundGame)" :disabled="controlBusy" @click="gameControl('stop')">停止设备会话</button><button @click="emit('game')">游玩预算与设置</button><p v-if="boundGame.state==='paused'">游玩暂停，可人工操作；普通聊天与记忆处理不会恢复设备。</p><p v-else-if="['pausing','stopping'].includes(boundGame.state)">设备操作正在收尾，人工仍锁定，等待已暂停或已结束。</p><p v-else-if="isActive(boundGame)">AI 持有设备控制；需要人工输入时先暂停并等待完成。</p><p v-if="boundGame.pause_reason">{{ boundGame.pause_reason.title }}：{{ boundGame.pause_reason.detail }} {{ boundGame.pause_reason.suggestion }}</p></div>
    <div ref="scroll" class="chat-scroll" @scroll="onScroll">
      <button v-if="moreBefore" :disabled="loadingHistory" @click="history">{{ loadingHistory ? '正在加载…' : '加载更早记录' }}</button>
      <div v-if="!turns.length && !waiting.length" class="empty"><h4>不连接设备也能开始</h4><p>讨论目标、整理攻略、维护本包记忆，或引导已有游玩会话。公开输出和真实工具收据会随处理进度更新。</p><button @click="emit('memory')">查看记忆库</button></div>
      <article v-for="turn in turns" :id="`agent-turn-${turn.id}`" :key="turn.id" class="turn">
        <div v-for="message in turn.users" :key="message.id" class="user-message"><header><b>你</b><small>{{ DELIVERY_LABELS[message.status] || message.status }}</small><button v-if="message.status === 'queued'" @click="withdraw(message)">撤回</button></header><p>{{ message.text }}</p></div>
        <details v-if="turn.process.length" class="process" :open="detailMode || !turn.completed || turn.process.some(item => item.status === 'failed')"><summary>思考与步骤 · {{ turn.process.length }} 项</summary>
          <div v-for="item in turn.process" :key="item.id" :class="['process-item',item.kind,item.status]">
            <div v-if="item.kind === 'thinking'"><header><b>{{ item.label }}</b><small v-if="item.status === 'streaming'">增量输出中</small></header><p>{{ item.text }}</p></div>
            <div v-else><header><b>{{ item.label }}</b><small>{{ ({running:'执行中',complete:'已有结果',failed:'失败'})[item.status] || item.status }}</small><span v-if="detailMode && item.stepId">步骤 {{ item.stepId }}</span></header><p>{{ item.message }}</p>
              <ul v-if="item.name?.startsWith('memory_') && item.receipt?.items" class="memory-hits"><li v-for="hit in item.receipt.items" :key="hit.id || hit.revision"><b>{{ hit.title || hit.id }}</b> · r{{ hit.revision }}<MemoryEvidence :memory="hit" /><small>{{ hit.summary || hit.reason }}<span v-if="hit.sources?.length"> · {{ hit.sources.length }} 个来源</span></small></li></ul>
              <p v-if="item.name?.startsWith('memory_') && item.receipt?.saved">{{ item.receipt.receipt_pending ? '正文已落盘，提交收据仍待修复' : '记忆提交已保存' }} · {{ item.receipt.id }}<span v-if="item.receipt.revision"> · r{{ item.receipt.revision }}</span></p>
              <template v-if="item.name?.startsWith('memory_') && item.receipt?.memory"><p>{{ item.receipt.memory.title }} · r{{ item.receipt.memory.revision }} · {{ item.receipt.memory.applicability || '按当前环境判断适用条件' }}</p><MemoryEvidence :memory="{...item.receipt.memory,source_conflicts:item.receipt.source_conflicts ?? item.receipt.memory.source_conflicts}" show-canonical /></template>
              <details><summary>参数与实际结果</summary><pre>{{ eventDetails(item.data) }}</pre><figure v-if="item.image"><img :src="item.image" alt="该步骤的历史观察截图" /><figcaption>历史观察，当前游戏画面可能已变化。</figcaption></figure></details></div>
          </div>
        </details>
        <div v-for="answer in turn.answers.filter(item=>item.text)" :key="answer.id" class="final-answer"><header><b>AI 答复</b><small>{{ answer.status === 'streaming' ? '正在输出' : answer.status === 'interrupted' ? '输出已中断' : turn.completed && answer===turn.answers.at(-1) ? '本轮最终答复' : '公开回答' }}</small></header><p>{{ answer.text }}</p></div>
        <div v-for="notice in turn.notices" :key="notice.id" class="notice"><p>{{ notice.text }}</p><button v-if="notice.data?.state === 'error'" @click="loadDiagnostics()">查看相关诊断</button></div>
      </article>
      <div v-for="message in waiting" :key="message.id" class="user-message pending"><header><b>你</b><small>{{ message.status === 'sending' ? '正在发送' : DELIVERY_LABELS[message.status] || message.status }}</small><button v-if="message.status === 'queued'" @click="withdraw(message)">撤回</button></header><p>{{ message.text }}</p></div>
    </div>
    <button v-if="!nearBottom" class="return-bottom" @click="bottom">回到最新</button>
    <div class="composer"><p v-if="error" role="alert" class="error">{{ error }}</p><p v-if="feedback" role="status">{{ feedback }}</p><p v-if="!saved?.has_key" class="hint">请先到游玩页的“模型”设置保存聊天 API。记忆库仍可只读查询。</p><p v-if="!packageId" class="hint">先选择配置包；聊天不需要设备。</p>
      <div v-if="attachedMemory.length" class="attachments"><span v-for="item in attachedMemory" :key="item.id">{{ item.title }} · r{{ item.revision }}<button aria-label="移除记忆引用" @click="emit('detach',item.id)">×</button></span></div>
      <p v-if="readonlyHistory" class="hint">此记录仅供回看。需要聊天时请在当前配置包建立新对话；历史输入不会重放。</p>
      <form @submit.prevent="send"><textarea v-model="draft" rows="3" aria-label="Agent 消息" placeholder="继续讨论，或指示 AI 管理记忆…" :disabled="sending || readonlyHistory" @keydown.ctrl.enter.prevent="send" />
        <div class="composer-tools"><label>游玩指令<select v-model="gameId" aria-label="本条消息关联游戏会话"><option value="">仅聊天与记忆</option><option v-for="session in eligibleSessions" :key="session.session_id" :value="session.session_id">{{ session.device_id }} · {{ stateLabel(session.state) }}</option></select></label><label v-if="gameSession"><input v-model="allowResume" type="checkbox" />明确按此指令继续 AI 游玩</label><button v-if="gameSession" type="button" @click="emit('game')">查看暂停/停止控制</button><label v-if="searchAvailable"><input v-model="webSearch" type="checkbox" />本条允许独立联网搜索</label><button type="submit" :disabled="!canSend">{{ sending ? '正在发送…' : '发送消息' }}</button></div>
      </form><small>消息先标记接收与排队，纳入后更新状态。取消问答、暂停游玩和停止设备会话各自生效。</small>
      <p v-if="conversation?.usage" class="usage">模型 {{ usageValue(conversation.usage,['turns']) }} / {{ budgetValue(conversation.limits,'max_turns') }} 轮 · 工具 {{ usageValue(conversation.usage,['actions']) }} / {{ budgetValue(conversation.limits,'max_actions') }} 次 · 活动 {{ usageValue(conversation.usage,['active_seconds']) }} / {{ budgetValue(conversation.limits,'max_seconds') }} 秒 · Token {{ tokenUsage(conversation.usage) }} / {{ budgetValue(conversation.limits,'max_tokens') }} · 连续失败 {{ usageValue(conversation.usage,['consecutive_failures']) }} / {{ budgetValue(conversation.limits,'max_failures') }}</p>
    </div>
    <aside v-if="diagnosticsOpen" class="diagnostic-drawer" aria-label="对话诊断"><header><h3>只读诊断</h3><button @click="diagnosticsOpen=false">关闭诊断</button></header><div class="actions"><select v-model="diagnosticFilter" aria-label="诊断类别"><option value="all">全部</option><option value="model">模型请求</option><option value="tool">工具</option><option value="memory">记忆</option><option value="state">状态</option><option value="error">错误</option></select><button @click="loadDiagnostics()">刷新诊断</button><button v-if="moreDiagnostics" @click="loadDiagnostics(true)">加载更早诊断</button><button :disabled="exportBusy" @click="exportDiagnostics">{{ exportBusy ? '正在导出…' : '导出脱敏诊断' }}</button></div><p>导出到本机，不调用模型、不上传。缺失用量、耗时或附件显示未知。</p><p v-if="diagnosticError" role="alert" class="error">{{ diagnosticError }}</p><ol><li v-for="event in filteredDiagnostics" :key="event.seq"><header><b>{{ event.kind }}</b><time>{{ displayTime(event.at) }}</time><button v-if="event.data?.turn_id" @click="locate(event)">定位对话</button></header><p>{{ event.message }}</p><pre>{{ eventDetails(event.data) }}</pre></li></ol></aside>
  </section>
</template>

<style scoped>
.memory-hits{padding-left:18px;line-height:1.8}.memory-hits small{display:block}
.game-controls{padding:10px 14px;display:flex;align-items:center;gap:8px;flex-wrap:wrap;border-bottom:1px solid var(--border,#41484a)}.game-controls p{width:100%;color:var(--text-2,#aab5b2)}
.chat-budget{padding:12px;border-bottom:1px solid var(--border,#41484a);max-height:180px;overflow:auto}.budget-grid{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:8px}.budget-grid label{display:grid;gap:5px}
.agent-conversation{display:flex;flex-direction:column;min-height:0;flex:1;position:relative;font-size:12px;overflow:hidden}header,.actions{display:flex;align-items:center;justify-content:space-between;gap:8px;flex-wrap:wrap}.agent-conversation>header,.conversation-bar{padding:10px 14px;border-bottom:1px solid var(--border,#41484a)}h3,h4,p{margin:0}p{white-space:pre-wrap;line-height:1.8;overflow-wrap:anywhere}small,.hint{color:var(--text-2,#aab5b2);line-height:1.7}.conversation-bar{display:flex;gap:8px;align-items:center;flex-wrap:wrap}.conversation-bar select{flex:1}.chat-scroll{flex:1;overflow:auto;min-height:0;padding:14px;scrollbar-gutter:stable}.empty{display:grid;gap:12px;margin:25px 0;color:var(--text-2,#aab5b2)}.turn{display:grid;gap:14px;margin-bottom:22px}.user-message{margin-left:20px;border:1px solid var(--border,#41484a);border-radius:8px;padding:10px;background:var(--bg-2,#282b2d)}.user-message p,.final-answer p{margin-top:8px}.process{padding:10px;border-left:2px solid var(--border,#41484a)}summary{cursor:pointer}.process-item{padding:10px 0;border-bottom:1px solid var(--border,#41484a)}.process-item.failed,.error{color:var(--danger,#ef9292)}.final-answer{padding:8px 0}.notice{padding:10px;border:1px solid var(--accent,#e4c956);border-radius:6px}.composer{padding:12px 14px;border-top:1px solid var(--border,#41484a);display:grid;gap:8px}.composer form{display:grid;gap:8px}.composer-tools{display:flex;gap:8px;flex-wrap:wrap;align-items:center}.composer-tools label{display:flex;gap:5px;align-items:center}.composer textarea{resize:vertical;min-height:60px;max-height:140px}.attachments{display:flex;gap:6px;flex-wrap:wrap}.attachments span{padding:4px 7px;border:1px solid var(--border,#41484a);border-radius:4px}.attachments button{border:0;padding:0 5px}input,select,textarea,button{font:inherit;color:inherit;background:var(--bg-1,#181b1c);border:1px solid var(--border,#41484a);border-radius:4px;padding:6px;min-width:0}button{cursor:pointer}button:disabled{opacity:.5;cursor:default}pre{white-space:pre-wrap;overflow-wrap:anywhere;padding:8px;background:var(--bg-1,#181b1c);font-size:11px;line-height:1.7;max-height:300px;overflow:auto}figure{margin:10px 0}figure img{max-width:100%;max-height:250px;object-fit:contain}.return-bottom{position:absolute;right:14px;bottom:180px}.diagnostic-drawer{position:absolute;inset:0;background:var(--bg-1,#181b1c);z-index:2;padding:16px;overflow:auto;display:block}.diagnostic-drawer>header{margin-bottom:12px}.diagnostic-drawer ol{list-style:none;padding:0;display:grid;gap:12px}.diagnostic-drawer li{border:1px solid var(--border,#41484a);padding:10px;border-radius:5px}:focus-visible{outline:2px solid var(--accent,#e4c956);outline-offset:2px}
</style>
