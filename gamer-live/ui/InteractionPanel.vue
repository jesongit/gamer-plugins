<script setup>
import { ref, reactive, computed, onMounted, onBeforeUnmount } from 'vue'
import { api } from '../../../web/src/api'
import { examples, fields, newRule, fixedValue } from './interaction-examples'
const props = defineProps({ devices: { type: Array, default: () => [] } })
const packages = ref([]), packageId = ref(''), deviceId = ref(''), loadedPackage = ref('')
const rules = ref([]), version = ref(null), entries = ref([]), schemas = reactive({}), editing = ref('')
const queue = ref({ waiting: [], history: [], receipts: [], paused: true, enabled: false }), selected = ref([])
const error = ref(''), feedback = ref(''), busy = ref(false), dirty = ref(false), historyOffset = ref(0), historyFilter = ref('')
const invalidInputs = reactive({})
const exampleIndex = ref(0), exampleHint = ref(''), preview = ref(null)
const simulation = reactive({ kind: 'message', text: '跳', gift_id: '', gift_name: '', count: 1 })
const log = ref(null), logLoading = ref(false)
const call = (action, values = {}) => api.callExtension('gamer-live', action, values)
const stateLabels = { starting: '正在启动', running: '执行中', waiting: '等待中', cancelling: '正在取消', review: '结果待核对', acknowledged: '已核对结束', removed: '已移除', success: '成功', failed: '失败', cancelled: '已取消' }
const canBind = computed(() => !queue.value.enabled && !queue.value.current && !queue.value.waiting.length)
let timer, disposed = false, loadGeneration = 0
const date = ms => ms ? new Date(ms).toLocaleString() : '—'
const age = ms => `${Math.max(0, Math.floor((Date.now() - ms) / 1000))} 秒`
async function refresh() {
  const offset = historyOffset.value, filter = historyFilter.value
  const value = await call('queue.status', { offset, filter })
  if (!disposed && offset === historyOffset.value && filter === historyFilter.value && (value.revision >= (queue.value.revision || 0))) queue.value = value
}
async function poll() {
  try { if (!busy.value) await refresh() } catch (e) { if (!disposed) error.value = e.message }
  if (!disposed) timer = setTimeout(poll, 1500)
}
async function operate(fn, success = '操作完成') {
  if (busy.value) return
  busy.value = true; error.value = ''; feedback.value = ''
  try { await fn(); await refresh(); feedback.value = success } catch (e) { error.value = e.message } finally { busy.value = false }
}
async function control(op, ids = []) {
  await operate(async () => {
    const result = await call('queue.control', { op, ids, request_id: crypto.randomUUID() })
    if (result.results?.some(r => r.result?.includes('无法移除'))) throw new Error('部分项已开始或已结束，无法移除；请查看最新队列')
    selected.value = []
  })
}
async function loadRules() {
  const pkg = packageId.value, generation = ++loadGeneration
  const [saved, scripts, libraries] = await Promise.all([call('rules.read', { package_id: pkg }), api.listScripts(pkg), api.listFunctions(pkg)])
  if (disposed || generation !== loadGeneration) return
  rules.value = saved.rules; version.value = saved.version; loadedPackage.value = pkg; editing.value = ''; dirty.value = false
  for (const key of Object.keys(invalidInputs)) delete invalidInputs[key]
  entries.value = [
    ...libraries.flatMap(lib => lib.functions.map(f => ({ id: `${pkg}#${typeof f === 'string' ? f : f.name}`, name: `函数 · ${typeof f === 'string' ? f : f.name}` }))),
    ...scripts.map(s => ({ id: s.id, name: `自动化 · ${s.name}` })),
  ]
}
async function schemaFor(rule) {
  if (!rule.entrypoint) return
  const data = await api.getEntrypointParams('gamer-yaml', rule.entrypoint)
  schemas[rule.entrypoint] = data.schema || []
}
async function edit(rule) { editing.value = rule.id; await operate(() => schemaFor(rule), ''); }
function add(example) { const rule = newRule(example); rules.value.push(rule); editing.value = rule.id; exampleHint.value = example?.hint || ''; dirty.value = true }
function move(index, direction) { const next = index + direction; if (next < 0 || next >= rules.value.length) return; [rules.value[index], rules.value[next]] = [rules.value[next], rules.value[index]]; dirty.value = true }
async function chooseEntry(rule) { rule.args = {}; for (const key of Object.keys(invalidInputs)) if (key.startsWith(`${rule.id}:`)) delete invalidInputs[key]; dirty.value = true; await operate(() => schemaFor(rule), '') }
function source(rule, param, value) {
  delete invalidInputs[`${rule.id}:${param.name}`]
  if (value === 'default') delete rule.args[param.name]
  else rule.args[param.name] = { source: value, ...(value === 'event' ? { field: param.type === 'integer' ? 'count' : 'text' } : { value: param.default ?? (param.type === 'boolean' ? false : '') }) }
  dirty.value = true
}
function setFixed(rule, param, text) { const key = `${rule.id}:${param.name}`; try { rule.args[param.name].value = fixedValue(text, param.type); dirty.value = true; delete invalidInputs[key]; error.value = '' } catch (e) { invalidInputs[key] = true; error.value = `${param.name}：${e.message}` } }
function displayFixed(value) { return value != null && typeof value === 'object' ? JSON.stringify(value) : value ?? '' }
function logText(e) {
  if (typeof e === 'string') return e
  if (e.name === 'log') return e.data?.message || ''
  if (e.ev === 'run_start') return '开始执行'
  if (e.ev === 'run_end') return e.ok ? '执行完成' : `执行结束：${e.error || '未成功'}`
  if (e.ev === 'step_start') return `开始步骤：${e.desc || e.path}`
  if (e.ev === 'step_end') return `${e.ok ? '步骤完成' : '步骤失败'}：${e.path}${e.error ? ` · ${e.error}` : ''}`
  if (e.ev === 'call_start') return `调用：${e.target}`
  if (e.ev === 'vision') return `${e.template} · ${e.found ? '命中' : '未命中'}`
  if (e.message) return e.message
  return `${e.name || e.ev || '事件'} ${JSON.stringify(e.data || e)}`
}
async function save() {
  if (Object.keys(invalidInputs).some(key => rules.value.some(r => key.startsWith(`${r.id}:`)))) { error.value = '请先修正无效参数'; return }
  await operate(async () => {
    const result = await call('rules.save', { package_id: loadedPackage.value, expected_version: version.value, ruleset: { schema_version: 1, rules: rules.value } })
    version.value = result.version; dirty.value = false
  }, '规则已保存，已排队内容保持原入口和参数')
}
async function simulate(enqueue) {
  await operate(async () => {
    preview.value = await call(enqueue ? 'queue.test' : 'rules.preview', { kind: simulation.kind, request_id: crypto.randomUUID(), payload: { text: simulation.text, gift_id: simulation.gift_id, gift_name: simulation.gift_name, count: simulation.count } })
  }, enqueue ? '请查看模拟结果和执行队列' : '预览完成，未操作设备')
}
async function showLog(item) {
  if (!item.run_id) { log.value = { title: item.name, events: [], note: item.error || '此项尚未提交运行' }; return }
  logLoading.value = true; log.value = { title: item.name, events: [], note: '' }
  try {
    const viewer = log.value
    let after = 0
    do {
      const page = await api.getRunEvents(item.run_id, after)
      if (disposed || log.value !== viewer) return
      viewer.events.push(...(page.events || [])); if (!page.has_more) break
      const next = page.next
      if (!next || next <= after) throw new Error('日志分页游标无效')
      after = next
    } while (true)
  } catch (e) { error.value = e.message } finally { logLoading.value = false }
}
onMounted(async () => {
  try {
    const data = await api.listPackages(); packages.value = Array.isArray(data) ? data : data.packages || []
    await refresh(); deviceId.value = queue.value.target?.device_id || props.devices[0]?.id || ''; packageId.value = queue.value.target?.package_id || packages.value[0]?.id || ''
    if (packageId.value) await loadRules()
  } catch (e) { if (!disposed) error.value = e.message }
  if (!disposed) poll()
})
onBeforeUnmount(() => { disposed = true; clearTimeout(timer); loadGeneration++ })
</script>

<template>
  <div class="interaction-panel">
    <p v-if="error" class="error" role="alert">{{ error }}</p><p v-if="feedback" class="feedback" role="status">{{ feedback }}</p>
    <section>
      <div class="head"><h3>执行目标</h3><span>{{ queue.target ? '已绑定' : '尚未绑定' }}</span></div>
      <div class="row"><label>目标设备<select v-model="deviceId" :disabled="!canBind || busy"><option value="">选择设备</option><option v-for="d in devices" :key="d.id" :value="d.id">{{ d.name || d.id }}</option></select></label><label>配置包<select v-model="packageId" :disabled="!canBind || dirty || busy"><option value="">选择配置包</option><option v-for="p in packages" :key="p.id" :value="p.id">{{ p.name || p.id }}</option></select></label></div>
      <div class="actions"><button :disabled="busy || !canBind || !packageId || !deviceId || dirty" @click="operate(async () => { await call('queue.configure', { device_id: deviceId, package_id: packageId }); await loadRules() }, '目标已绑定，队列处于暂停状态')">绑定目标并读取规则</button><button :disabled="busy || dirty || !packageId" @click="operate(loadRules, '规则已读取')">读取规则</button></div>
      <p class="hint" v-if="queue.target">队列绑定：{{ queue.target.device_id }} · {{ queue.target.package_id }} · {{ queue.target.android_package }}。切换工作台页面不会改变此目标。</p><button v-if="queue.target" :disabled="busy || !canBind" @click="control('unbind')">解除目标绑定</button>
    </section>

    <section class="queue-section">
      <div class="queue-toolbar"><div class="head"><h3>运行队列</h3><span>{{ queue.paused ? '已暂停' : '运行中' }} · 等待 {{ queue.waiting.length }}/{{ queue.capacity || 100 }}</span></div>
        <div class="actions"><button :disabled="busy || !queue.target" @click="control(queue.enabled ? 'disable' : 'enable')">{{ queue.enabled ? '停用新触发' : '启用新触发' }}</button><button :disabled="busy || !queue.target" @click="control(queue.paused ? 'resume' : 'pause')">{{ queue.paused ? '继续队列' : '暂停队列' }}</button><button class="danger" :disabled="busy" @click="control('stop')">停止互动执行</button></div>
      </div>
      <p class="hint">暂停只阻止下一项启动，仍可接收新互动。停止互动执行会停用新触发、清空等待项并取消当前互动运行。</p>
      <p v-if="queue.blocked" class="error" role="status">{{ queue.blocked }}</p>
      <div v-if="queue.current" class="current"><strong>{{ stateLabels[queue.current.state] }} · {{ queue.current.name }}</strong><span>已运行 {{ age(queue.current.started_at || queue.current.created_at) }}</span><p v-if="queue.current.error" class="error">{{ queue.current.error }}</p><div class="actions"><button @click="showLog(queue.current)">运行日志</button><button v-if="queue.current.state !== 'review'" :disabled="busy || queue.current.state === 'cancelling'" @click="control('cancel')">取消当前运行</button><button v-else :disabled="busy" @click="control('resolve', [queue.current.id])">已核对，结束此项</button></div></div>
      <p v-else-if="queue.device_run" class="hint">等待设备当前运行结束：{{ queue.device_run.entrypoint }}。手动或定时运行不属于本互动队列。</p><p v-else class="hint">当前没有互动运行。</p>
      <div class="actions"><button :disabled="busy || !selected.length" @click="control('remove', selected)">移除所选 {{ selected.length || '' }}</button><button :disabled="busy || !queue.waiting.length" @click="control('clear')">清空等待项</button></div>
      <ol class="queue-list"><li v-for="(item, index) in queue.waiting" :key="item.id"><div class="head"><label class="check"><input v-model="selected" type="checkbox" :value="item.id" />{{ index + 1 }}. {{ item.name }}</label><button :disabled="busy" @click="control('remove', [item.id])">移除</button></div><p class="hint">{{ item.event.test ? '模拟测试' : item.event.message?.actor?.name || '匿名观众' }} · 等待 {{ age(item.created_at) }}</p><details><summary>入口与参数</summary><p>{{ item.entrypoint }}</p><pre>{{ JSON.stringify(item.args, null, 2) }}</pre></details></li></ol>
      <p v-if="!queue.waiting.length" class="hint">等待队列为空。</p>
    </section>

    <details class="box" open><summary>互动规则 <span v-if="dirty">· 有未保存修改</span></summary>
      <p class="hint">按列表顺序匹配第一条命中的规则。示例默认停用，绑定真实入口并保存后才能启用。</p>
      <div class="actions"><select v-model.number="exampleIndex" aria-label="常用互动示例"><option v-for="(ex, i) in examples" :key="ex.name" :value="i">{{ ex.name }}</option></select><button :disabled="!loadedPackage || busy" @click="add(examples[exampleIndex])">添加示例</button><button :disabled="!loadedPackage || busy" @click="add()">新增规则</button><button :disabled="!dirty || busy" @click="save">保存规则</button><button :disabled="!dirty || busy" @click="operate(loadRules, '已放弃未保存修改')">放弃修改</button></div><p class="hint">{{ exampleHint }}</p>
      <article v-for="(rule, index) in rules" :key="rule.id" class="rule" @change="dirty = true">
        <div class="head"><label class="check"><input v-model="rule.enabled" type="checkbox" :disabled="busy" />{{ rule.name }}</label><div class="actions"><button :disabled="index === 0 || busy" @click="move(index, -1)">上移</button><button :disabled="index === rules.length - 1 || busy" @click="move(index, 1)">下移</button><button :disabled="busy" @click="editing === rule.id ? editing = '' : edit(rule)">{{ editing === rule.id ? '收起' : '编辑' }}</button><button :disabled="busy" @click="rules.splice(index, 1); dirty = true">删除</button></div></div>
        <div v-if="editing === rule.id" class="rule-editor" :inert="busy || undefined"><label>规则名称<input v-model="rule.name" maxlength="80" /></label><div class="row"><label>事件<select v-model="rule.kind"><option value="message">弹幕</option><option value="gift">礼物</option></select></label><label v-if="rule.kind === 'message'">条件<select v-model="rule.operator"><option value="equals">等于</option><option value="contains">包含</option></select></label></div>
          <label>{{ rule.kind === 'message' ? '弹幕内容' : '礼物 ID（从最近消息查看）' }}<input v-model="rule.value" maxlength="500" /></label><label v-if="rule.kind === 'gift'">单条消息最少数量<input v-model.number="rule.min_count" type="number" min="1" /></label>
          <label>执行函数或自动化<select v-model="rule.entrypoint" @change="chooseEntry(rule)"><option value="">选择真实执行入口</option><option v-for="entry in entries" :key="entry.id" :value="entry.id">{{ entry.name }}</option><option v-if="rule.entrypoint && !entries.some(e => e.id === rule.entrypoint)" :value="rule.entrypoint">{{ rule.entrypoint }}（入口不可用）</option></select></label>
          <div v-for="param in schemas[rule.entrypoint] || []" :key="param.name" class="parameter"><label>{{ param.name }} · {{ param.type }}{{ param.required ? ' · 必填' : '' }}<small>{{ param.desc }}</small><select :value="rule.args[param.name]?.source || 'default'" @change="source(rule, param, $event.target.value)"><option value="default">使用入口默认值／不传入</option><option value="fixed">固定值</option><option value="event">事件字段</option></select></label>
            <select v-if="rule.args[param.name]?.source === 'event'" v-model="rule.args[param.name].field"><option v-for="[key, label] in fields" :key="key" :value="key">{{ label }}</option></select>
            <select v-else-if="rule.args[param.name]?.source === 'fixed' && param.type === 'boolean'" :value="String(rule.args[param.name].value)" @change="setFixed(rule, param, $event.target.value)"><option value="true">是</option><option value="false">否</option></select>
            <input v-else-if="rule.args[param.name]?.source === 'fixed'" :value="displayFixed(rule.args[param.name].value)" @change="setFixed(rule, param, $event.target.value)" :placeholder="['array','list','object','point','any'].includes(param.type) ? 'JSON 格式' : '参数值'" />
          </div><div class="row"><label>冷却秒数（0 关闭）<input v-model.number="rule.cooldown_secs" type="number" min="0" max="86400" /></label><label>执行超时秒数（0 不限制）<input v-model.number="rule.timeout_secs" type="number" min="0" max="86400" /></label></div>
        </div>
      </article><p v-if="!rules.length" class="hint">还没有规则，可以从常用示例添加。</p>
    </details>

    <details class="box"><summary>模拟事件</summary><p class="hint">使用已保存规则。预览不操作设备；加入队列测试会实际执行。</p><div class="row"><label>类型<select v-model="simulation.kind"><option value="message">弹幕</option><option value="gift">礼物</option></select></label><label v-if="simulation.kind === 'message'">内容<input v-model="simulation.text" /></label><template v-else><label>礼物 ID<input v-model="simulation.gift_id" /></label><label>数量<input v-model.number="simulation.count" type="number" min="1" /></label></template></div><div class="actions"><button :disabled="busy || dirty || !queue.target" @click="simulate(false)">仅预览匹配</button><button :disabled="busy || dirty || !queue.target" @click="simulate(true)">加入队列测试（实际执行）</button></div><div v-if="preview"><p>{{ preview.result }}</p><pre v-if="preview.resolved">{{ JSON.stringify(preview.resolved, null, 2) }}</pre></div></details>

    <details class="box"><summary>最近执行结果</summary><div class="actions"><select v-model="historyFilter" @change="historyOffset = 0; operate(refresh, '')"><option value="">全部结果</option><option v-for="key in ['success','failed','cancelled','removed','acknowledged']" :key="key" :value="key">{{ stateLabels[key] }}</option></select><button :disabled="busy || historyOffset === 0" @click="historyOffset -= 30; operate(refresh, '')">上一页</button><button :disabled="busy || !queue.has_more" @click="historyOffset += 30; operate(refresh, '')">下一页</button></div><article v-for="item in queue.history" :key="item.id" class="result"><strong>{{ item.name }} · {{ stateLabels[item.state] }}</strong><p class="hint">{{ date(item.finished_at) }}</p><p v-if="item.error" class="error">{{ item.error }}</p><div class="actions"><button @click="showLog(item)">详情与日志</button><button v-if="['failed','cancelled','acknowledged'].includes(item.state)" :disabled="busy" @click="control('retry', [item.id])">重新执行并排到队尾</button></div></article></details>
    <details class="box"><summary>最近触发处理</summary><div class="receipts"><p v-for="(r, index) in queue.receipts" :key="index">{{ date(r.at) }} · {{ r.event.actor?.name || '匿名观众' }} · {{ r.event.payload?.text || r.event.payload?.gift_name || r.event.kind }} · {{ r.rule || '未命中规则' }} · {{ r.result }}</p></div></details>
    <section v-if="log"><div class="head"><h3>{{ log.title }} · 运行日志</h3><button @click="log = null">关闭</button></div><p v-if="logLoading">读取日志中…</p><p>{{ log.note }}</p><div class="log-lines"><p v-for="(event, index) in log.events" :key="index"><time v-if="event.time">{{ new Date(event.time).toLocaleTimeString() }} · </time>{{ logText(event) }}</p></div></section>
  </div>
</template>

<style scoped>
.interaction-panel{display:grid;gap:16px;min-width:0}section,.box{border:1px solid var(--border,#41444c);border-radius:10px;padding:16px;display:grid;gap:12px;min-width:0}.box>summary{cursor:pointer;font-size:14px;font-weight:600}.box[open]>summary{margin-bottom:12px}.box>*+*{margin-top:12px}.head,.actions,.row{display:flex;gap:8px;align-items:center;flex-wrap:wrap}.head{justify-content:space-between}.head span,.hint,small{font-size:12px;opacity:.7;line-height:1.6}.row>label{flex:1;min-width:120px}h3,p{margin:0}h3{font-size:14px}label,.rule-editor,.parameter{display:grid;gap:6px;font-size:13px}.rule-editor{gap:12px;margin-top:12px}.rule,.current,.result{padding:12px 0;border-top:1px solid var(--border,#41444c);display:grid;gap:8px}.check{display:flex;align-items:center;gap:6px}.check input{width:auto}input,select,button{box-sizing:border-box;min-width:0;max-width:100%;padding:7px 9px;border:1px solid var(--border,#535762);border-radius:5px;background:var(--bg-1,#24262c);color:inherit;font-size:13px}input,select{width:100%}.actions select{width:auto;flex:1}button{cursor:pointer}button:disabled{opacity:.45;cursor:default}.danger,.error{color:#f19494}.feedback{color:#77cbb4}.queue-list{list-style:none;margin:0;padding:0;max-height:400px;overflow:auto}.queue-list li{padding:10px 0;border-bottom:1px solid var(--border,#41444c)}pre{white-space:pre-wrap;overflow-wrap:anywhere;font-size:12px;margin:6px 0}.receipts,.log-lines{max-height:320px;overflow:auto;font-size:12px;line-height:1.7}details summary{cursor:pointer}section,article,p{overflow-wrap:anywhere}.queue-toolbar{display:grid;gap:10px}
</style>
