<script setup>
import { ref, onMounted, onBeforeUnmount } from 'vue'
import { api } from '../../../web/src/api'
import { badges, eventText, giftDetails, safeImage, states, kinds } from './live-log-format'
defineProps({ busy: Boolean })
const emit = defineEmits(['run-log', 'retry'])
const rows = ref([]), filter = ref('trigger'), search = ref(''), frozen = ref(false), pending = ref(0), selected = ref(null)
const error = ref(''), more = ref(false), next = ref(0), loading = ref(false), oldest = ref(null)
let disposed = false, timer, generation = 0, searchTimer, latest = 0
const gap = ref(false)
const time = ms => ms ? new Date(ms).toLocaleString() : '—'
function status(row) { return row.item ? states[row.item.state] || row.item.state : row.result }
async function read(append = false) {
  if (loading.value) return
  loading.value = true
  const gen = generation
  try {
    const page = await api.callExtension('gamer-live', 'logs.read', { before: append ? next.value : 0, after: append || filter.value === 'errors' ? 0 : latest, filter: filter.value, search: search.value, refresh: rows.value.map(r => r.seq) })
    if (disposed || gen !== generation) return
    error.value = ''; oldest.value = page.oldest; gap.value ||= !!page.gap
    const existing = new Map(rows.value.map(r => [r.seq, r]))
    for (const r of page.updates || []) if (existing.has(r.seq)) existing.set(r.seq, r)
    if (frozen.value && !append) pending.value = page.rows.filter(r => !existing.has(r.seq)).length
    else { for (const r of page.rows) existing.set(r.seq, r); pending.value = 0 }
    rows.value = [...existing.values()].sort((a, b) => b.seq - a.seq).slice(0, 500)
    if (selected.value) selected.value = existing.get(selected.value.seq) || selected.value
    if (!append && !frozen.value) latest = page.next_after ?? Math.max(latest, ...page.rows.map(r => r.seq))
    if (append || !next.value) { next.value = page.next; more.value = page.has_more }
  } catch (e) { if (gen === generation) error.value = e.message } finally { loading.value = false }
}
async function reset() { generation++; latest = 0; rows.value = []; next.value = 0; more.value = false; pending.value = 0; if (!loading.value) await read() }
function find() { clearTimeout(searchTimer); searchTimer = setTimeout(reset, 250) }
async function poll() { await read(); if (!disposed) timer = setTimeout(poll, 1500) }
function resume() { frozen.value = false; read() }
onMounted(poll)
onBeforeUnmount(() => { disposed = true; generation++; clearTimeout(timer); clearTimeout(searchTimer) })
</script>
<template>
  <div class="journal">
    <div class="toolbar"><select v-model="filter" aria-label="日志筛选" @change="reset"><option value="trigger">触发相关</option><option value="all">全部互动</option><option value="rejected">未入队</option><option value="errors">执行异常</option><option value="system">房间与系统</option></select><input v-model="search" aria-label="搜索互动日志" placeholder="搜索昵称、内容、规则" @input="find" /><button @click="frozen ? resume() : frozen = true">{{ frozen ? `继续更新${pending ? ' · 新记录 ' + pending + '+' : ''}` : '暂停列表滚动' }}</button></div>
    <p class="hint">最近 7 天，最多 10,000 条；存储达到上限时会提前清理。暂停列表仅固定阅读位置，队列继续执行。</p>
    <p v-if="error" class="error" role="alert">{{ error }}</p>
    <p v-if="gap" class="hint">部分新消息已超出保留范围；目前显示仍可读取的记录。</p>
    <p v-if="oldest && rows.some(r => r.seq < oldest)" class="hint">部分旧记录已超出服务端保留范围，当前保留阅读快照。</p>
    <p v-if="!rows.length" class="empty">{{ loading ? '读取中…' : '暂无符合条件的互动。普通聊天可在「全部互动」查看。' }}</p>
    <article v-for="row in rows" :key="row.seq" :class="{ bad: ['failed','review'].includes(row.item?.state) }">
      <button class="record" :aria-expanded="selected?.seq === row.seq" @click="selected = selected?.seq === row.seq ? null : row">
        <span class="meta"><time>{{ time(row.at) }}</time><span>{{ kinds[row.event.kind] || row.event.kind }}</span><span v-if="row.item?.event?.test">模拟测试</span></span>
        <span class="main"><strong>{{ row.event.kind === 'message.mirror' ? '跨房消息' : row.event.actor?.name || '未提供观众信息' }}</strong><span v-for="badge in badges(row.event.actor)" :key="badge" class="badge">{{ badge }}</span></span>
        <span class="content">{{ eventText(row.event) }}</span><span class="outcome"><span>{{ row.rule || '—' }}<template v-if="row.item"> · #{{ row.item.number }}</template></span><b :class="row.item?.state">{{ status(row) }}</b></span>
      </button>
      <div v-if="selected?.seq === row.seq" class="detail">
        <p v-if="badges(row.event.actor, true).length">{{ badges(row.event.actor, true).join(' · ') }}</p>
        <p v-if="row.withdrawn">该醒目留言已被撤回。</p>
        <p v-else-if="row.event.kind === 'super_chat' && row.event.payload.end_time">展示截止：{{ time(row.event.payload.end_time * 1000) }}</p>
        <p v-if="row.event.payload.reply_name">回复 {{ row.event.payload.reply_name }}</p>
        <img v-if="safeImage(row.event.payload.emoji_url)" :src="safeImage(row.event.payload.emoji_url)" alt="表情弹幕" referrerpolicy="no-referrer" />
        <p v-if="giftDetails(row.event.payload).length">{{ giftDetails(row.event.payload).join(' · ') }}</p>
        <p v-if="row.event.kind === 'gift'">礼物 ID：{{ row.event.payload.gift_id }}</p>
        <ol class="timeline"><li>{{ time(row.at) }} · 收到互动</li><li>{{ row.rule ? '命中「' + row.rule + '」' : '规则判定' }} · {{ row.result }}</li><li v-if="row.item">{{ time(row.item.created_at) }} · 加入队列 #{{ row.item.number }}</li><li v-if="row.item?.started_at">{{ time(row.item.started_at) }} · 开始执行</li><li v-if="row.item?.finished_at">{{ time(row.item.finished_at) }} · {{ status(row) }}</li></ol>
        <p v-if="row.item?.error" class="error">{{ row.item.error }}</p>
        <p v-if="row.item?.retry_of" class="hint">这是一次手动重试，已重新排到队尾。</p>
        <div v-if="row.item" class="toolbar"><button @click="emit('run-log', row.item)">查看运行日志</button><button v-if="['failed','cancelled','acknowledged'].includes(row.item.state)" :disabled="busy" @click="emit('retry', row.item)">重新执行并排到队尾</button></div>
      </div>
    </article>
    <button v-if="more && rows.length < 500" :disabled="loading" @click="read(true)">加载更早记录</button><p v-if="rows.length >= 500" class="hint">已显示 500 条，请用搜索或筛选缩小范围。</p>
  </div>
</template>
<style scoped>
.journal{display:grid;gap:10px;min-width:0}.toolbar,.meta,.main,.outcome{display:flex;align-items:center;gap:8px;flex-wrap:wrap}.toolbar input{flex:1;min-width:120px}.hint,.meta{font-size:12px;color:var(--text-2,#9aa6b5);line-height:1.6}.record{display:grid;gap:8px;width:100%;text-align:left;padding:14px;border:0;background:transparent;color:inherit;cursor:pointer}.main{font-size:14px}.badge{font-size:11px;padding:2px 6px;border-radius:4px;background:#278a7426;color:#77cbb4}.outcome{justify-content:space-between;font-size:12px}.outcome b{font-weight:500;color:#9fb5c8}.outcome .success{color:#77cbb4}.outcome .running,.outcome .waiting{color:#e5c480}.content{font-size:14px;line-height:1.6}article{border:1px solid var(--border,#41444c);border-radius:8px;overflow-wrap:anywhere}.bad{border-color:#905757}.detail{border-top:1px solid var(--border,#41444c);padding:14px;display:grid;gap:10px;font-size:13px}.detail img{max-width:72px;max-height:72px}.timeline{padding-left:20px;margin:0;line-height:1.9}.error{color:#f19494}.empty{padding:24px 10px;opacity:.7;text-align:center}p{margin:0}input,select,.toolbar button,.journal>button{background:var(--bg-1,#24262c);color:inherit;border:1px solid var(--border,#41444c);border-radius:5px;padding:7px;font-size:12px;max-width:100%;box-sizing:border-box}button:disabled{opacity:.45}
</style>
