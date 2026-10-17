<script setup>
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import { api } from '../../../web/src/api'
const users = ref([]), search = ref(''), busy = ref(false), loading = ref(false), loaded = ref(false)
const error = ref(''), feedback = ref(''), hasMore = ref(false), nextOffset = ref(0)
const settings = reactive({ default_quota: 1, version: null }), defaultQuota = ref(1)
const editing = ref(''), draft = reactive({ balance: 0, unlimited: false })
let generation = 0, disposed = false
const call = (action, values = {}) => api.callExtension('gamer-live', action, values)
const selected = computed(() => users.value.find(user => user.id === editing.value))
const validBalance = computed(() => Number.isSafeInteger(draft.balance) && draft.balance >= (selected.value?.reserved || 0))
async function load(append = false) {
  const current = ++generation; loading.value = true; error.value = ''; feedback.value = ''
  try {
    const result = await call('users.read', { search: search.value, offset: append ? nextOffset.value : 0 })
    if (disposed || current !== generation) return
    users.value = append ? [...users.value, ...result.users] : result.users
    hasMore.value = result.has_more === true; nextOffset.value = result.next_offset ?? 0
    if (result.settings) { Object.assign(settings, result.settings); defaultQuota.value = settings.default_quota }
    loaded.value = true; if (!append) editing.value = ''
  } catch (e) { if (!disposed && current === generation) error.value = `用户未读取：${e.message}` }
  finally { if (current === generation) loading.value = false }
}
function edit(user) { editing.value = user.id; draft.balance = user.balance; draft.unlimited = user.unlimited; error.value = ''; feedback.value = '' }
async function saveUser() {
  if (busy.value || !selected.value || !validBalance.value) return
  const user = selected.value; busy.value = true; error.value = ''; feedback.value = ''
  try {
    const result = await call('users.update', { id: user.id, balance: draft.balance, unlimited: draft.unlimited, expected_version: user.version, request_id: crypto.randomUUID() })
    users.value = users.value.map(value => value.id === result.id ? result : value)
    editing.value = ''; feedback.value = '用户额度已保存。'
  } catch (e) { error.value = `额度未保存：${e.message}。可重新读取后重试。` }
  finally { busy.value = false }
}
async function saveDefault() {
  if (busy.value || !Number.isSafeInteger(defaultQuota.value) || defaultQuota.value < 0) return
  busy.value = true; error.value = ''; feedback.value = ''
  try { Object.assign(settings, await call('users.settings.save', { default_quota: defaultQuota.value, expected_version: settings.version })); feedback.value = '新人默认额度已保存，已有用户的余额保持原值。' }
  catch (e) { error.value = `默认额度未保存：${e.message}` }
  finally { busy.value = false }
}
onMounted(() => load())
onBeforeUnmount(() => { disposed = true; generation++ })
</script>

<template>
  <section id="live-users" class="users-panel" aria-label="用户管理">
    <div class="head"><h3>用户管理</h3><span class="hint">按观众累计使用额度</span></div>
    <form class="default-form" @submit.prevent="saveDefault"><label>新人默认额度<input v-model.number="defaultQuota" aria-label="新人默认额度" type="number" min="0" step="1" :disabled="busy || !loaded" /></label><button type="submit" :disabled="busy || !loaded || defaultQuota === settings.default_quota || !Number.isSafeInteger(defaultQuota) || defaultQuota < 0">保存默认额度</button></form>
    <p class="hint">默认 1 次。修改后只影响新加入的观众。排队时预占额度，成功后扣除；失败或取消会释放预占。</p>
    <form class="search-form" @submit.prevent="load()"><label class="search-label">查找观众<input v-model="search" aria-label="搜索用户名或 open_id" placeholder="用户名或完整 open_id" type="search" :disabled="busy" /></label><button type="submit" :disabled="loading || busy">搜索</button><button type="button" :disabled="loading || busy" @click="load()">刷新</button></form>
    <p v-if="loading" class="hint" role="status">正在读取用户…</p>
    <div v-if="users.length" class="table-scroll" tabindex="0" aria-label="用户额度列表">
      <table><thead><tr><th scope="col">观众</th><th scope="col">余额</th><th scope="col">预占</th><th scope="col">可用</th><th scope="col"><span class="sr-only">操作</span></th></tr></thead><tbody>
        <template v-for="user in users" :key="user.id">
          <tr><th scope="row"><span class="name">{{ user.name || '未命名观众' }}</span><span v-if="user.medal_name" class="medal">{{ user.medal_name }} Lv{{ user.medal_level || 0 }}</span><span class="open-id">{{ user.open_id }}</span><span v-if="user.test" class="hint">模拟用户</span></th><td>{{ user.unlimited ? '∞' : user.balance }}</td><td>{{ user.reserved }}</td><td>{{ user.unlimited ? '∞' : user.available }}</td><td><button type="button" :disabled="busy" :aria-label="`编辑 ${user.name || user.open_id} 额度`" @click="edit(user)">编辑</button></td></tr>
          <tr v-if="editing === user.id"><td colspan="5"><form class="user-editor" @submit.prevent="saveUser"><label>余额<input v-model.number="draft.balance" aria-label="用户余额" type="number" :min="user.reserved" step="1" :disabled="busy" :aria-invalid="!validBalance" /></label><label class="check"><input v-model="draft.unlimited" type="checkbox" :disabled="busy" />不限次数</label><p class="hint">余额至少为已预占的 {{ user.reserved }} 次。无限次数用户仍显示实际预占。</p><div class="actions"><button type="submit" :disabled="busy || !validBalance">保存用户额度</button><button type="button" :disabled="busy" @click="editing = ''">取消编辑</button></div></form></td></tr>
        </template>
      </tbody></table>
    </div>
    <p v-else-if="loaded && !loading" class="hint">{{ search ? '没有找到匹配的观众。' : '观众触发互动后，会出现在这里。' }}</p>
    <button v-if="hasMore" type="button" :disabled="loading || busy" @click="load(true)">加载更多用户</button>
    <p v-if="feedback" class="feedback" role="status">{{ feedback }}</p><p v-if="error" class="error" role="alert">{{ error }}</p>
  </section>
</template>

<style scoped>
.users-panel{display:grid;gap:12px;min-width:0;padding:12px;border:1px solid var(--border,#3b3d42);border-radius:4px;color:var(--text-0,#eeece6);font-size:13px}h3,p{margin:0}h3{font-size:14px}.head,.actions,.default-form,.search-form{display:flex;align-items:center;gap:8px;flex-wrap:wrap}.head{justify-content:space-between}.default-form,.search-form{align-items:end}.default-form label{max-width:150px}.search-label{flex:1;min-width:160px}label{display:grid;gap:5px}.hint,.open-id,.medal{font-size:12px;line-height:1.5;color:var(--text-2,#b6b6ae)}input,button{box-sizing:border-box;max-width:100%;min-width:0;border:1px solid var(--control-border,#7d7b75);border-radius:3px;color:inherit;font:inherit;padding:6px 8px}input{width:100%;background:var(--field,#191b1f);caret-color:var(--accent,#dda951)}button{background:var(--bg-3,#34363b);cursor:pointer;justify-self:start}button:hover:not(:disabled){background:var(--border,#3b3d42)}button:disabled,input:disabled{opacity:.55;cursor:default}.table-scroll{overflow:auto;min-width:0;scrollbar-color:#55575c var(--field,#191b1f)}table{border-collapse:collapse;width:100%;table-layout:fixed;text-align:left;font-variant-numeric:tabular-nums}th,td{padding:9px 5px;border-bottom:1px solid var(--border,#3b3d42);vertical-align:top}thead th{font-size:12px;font-weight:500;color:var(--text-2,#b6b6ae)}th:first-child{width:48%}th:last-child{width:54px}tbody th{font-weight:400}.name,.open-id,.medal{display:block;overflow-wrap:anywhere}.name{font-weight:500}.open-id{margin-top:3px}.user-editor{display:grid;gap:8px}.user-editor>label:first-child{max-width:180px}.check{display:flex;align-items:center;gap:6px}.check input{width:auto}.feedback{color:var(--ok,#8fc9ae)}.error{color:var(--danger,#ed9696);overflow-wrap:anywhere}.sr-only{position:absolute;width:1px;height:1px;overflow:hidden;clip-path:inset(50%)}:is(input,button,.table-scroll):focus-visible{outline:2px solid var(--accent,#dda951);outline-offset:2px}::selection{background:var(--accent,#dda951);color:var(--accent-ink,#201d16)}@media(max-width:400px){th:first-child{width:44%}th,td{padding-inline:3px}.table-scroll button{padding:5px}.head{align-items:baseline}}
</style>
