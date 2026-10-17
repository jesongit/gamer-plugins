<script setup>
import { computed, inject, onBeforeUnmount, ref, watch } from 'vue'
import { api } from '../../../../../../web/src/api'
import { WORKSPACE_CONTEXT_KEY } from '../../../../../../web/src/workspace/context'
import { pluginMessageChannel } from '../../../../../../web/src/workspace/plugin-messages'
import { useConfirmDialog } from '../../../../../../web/src/components/ui/useConfirmDialog'
import { sourceDiff, readAuthoringLocation, rememberAuthoringLocation } from './generation-state'
import { inspectSampleArchive } from '../../../../../gamer-video/ui/src/components/video/sampleBundle'
import { videoApi } from '../../../../../gamer-video/ui/src/components/video/videoApi'
import { notifyYamlResourcesChanged, yamlResourcesChanged } from './resource-changes'
const props = defineProps({ packageId: { type: String, default: '' } })
const emit = defineEmits(['editing-state', 'show-logs'])
const workspace = inject(WORKSPACE_CONTEXT_KEY, null), confirm = useConfirmDialog()
const call = (action, values = {}) => api.callExtension('gamer-yaml', action, values)
const scripts = ref([]), selected = ref(''), doc = ref(null), version = ref(''), filename = ref(''), creating = ref(false)
const instruction = ref(''), source = ref(''), baseline = ref(''), samples = ref([]), templates = ref([]), chosenSamples = ref([]), chosenTemplates = ref([])
const busy = ref(false), error = ref(''), notice = ref(''), testArgs = ref('{}')
const loadingScripts = ref(false), loadingMaterials = ref(false)
const importing = ref(false), libraryScripts = ref([]), importId = ref('')
const testRun = ref(null), testDevice = ref(''), testTarget = ref(''), testState = ref('')
const repairRun = ref(''), repairSamples = ref([])
const request = pluginMessageChannel('gamer-yaml:open-generation')
let scope = 0, serial = 0, refreshSerial = 0, timer, disposed = false
const dirty = computed(() => source.value !== baseline.value)
const running = computed(() => doc.value?.job?.state === 'generating')
const testing = computed(() => ['starting', 'running', 'paused', 'stopping'].includes(testState.value))
const lifecycleBusy = computed(() => busy.value || running.value || testing.value)
const current = computed(() => doc.value?.versions.find(v => v.version === version.value))
const diagnostics = computed(() => dirty.value ? [] : current.value?.diagnostics || [])
function accept(value, latest = false) {
  const retainEdits = dirty.value && !latest
  doc.value = value
  if (latest || !value.versions.some(v => v.version === version.value)) version.value = value.head_version || ''
  if (!retainEdits) { baseline.value = value.versions.find(v => v.version === version.value)?.yaml || ''; source.value = baseline.value }
  rememberAuthoringLocation(props.packageId, { scriptId: value.script_id })
}
async function refresh() {
  const token = scope, pkg = props.packageId, query = ++refreshSerial
  if (!pkg) return
  loadingScripts.value = true; loadingMaterials.value = true
  const currentScope = () => token === scope && query === refreshSerial && !disposed
  const scriptLoad = call('authoring.list', { package_id: pkg }).then(async result => {
    if (!currentScope()) return
    scripts.value = result.scripts || []
    loadingScripts.value = false
    const requested = request.packageId === pkg && (request.repairRun || request.scriptId)
    if (!selected.value && !busy.value && !requested) {
      const remembered = readAuthoringLocation(pkg).scriptId
      const restored = scripts.value.find(item => item.script_id === remembered) || scripts.value.find(item => item.head_version)
      if (restored) await selectScript(restored.script_id)
    }
  }).catch(e => { if (currentScope()) error.value = e.message })
    .finally(() => { if (currentScope()) loadingScripts.value = false })
  const sampleLoad = api.callExtension('gamer-video', 'sample.list', {}).then(result => { if (currentScope()) samples.value = result.samples || [] })
    .catch(() => { if (currentScope()) samples.value = [] })
  const templateLoad = api.listTemplates(pkg).then(result => { if (currentScope()) templates.value = result || [] })
    .catch(() => { if (currentScope()) templates.value = [] })
  const libraryLoad = Promise.resolve().then(() => api.listScripts(pkg)).then(result => { if (currentScope()) libraryScripts.value = result || [] })
    .catch(() => { if (currentScope()) libraryScripts.value = [] })
  void Promise.allSettled([sampleLoad, templateLoad]).then(() => { if (currentScope()) loadingMaterials.value = false })
  // Finish draft operations independently of model readiness and shared archive loading.
  await Promise.allSettled([scriptLoad, libraryLoad])
}
async function importSample(event) {
  const file = event.target.files?.[0], pkg = props.packageId, token = scope
  event.target.value = ''
  if (!file || busy.value || running.value) return
  busy.value = true; error.value = ''
  try {
    const bundle = await inspectSampleArchive(file)
    if (token !== scope) return
    await videoApi.importSample(bundle.bytes)
    if (token === scope) await refresh()
  } catch (e) { if (token === scope) error.value = e.message }
  finally { if (token === scope) busy.value = false }
}
async function beforeTabChange() {
  const token = scope, snapshot = source.value
  if (busy.value) return false
  if (dirty.value && !await confirm('当前源码修改尚未保存，放弃这些修改？', { title: '未保存修改', confirmText: '放弃修改', danger: true })) return false
  if (token !== scope || snapshot !== source.value || busy.value) return false
  source.value = baseline.value; return true
}
async function selectScript(id) {
  if (!await beforeTabChange()) return
  const token = scope, op = ++serial
  selected.value = id; doc.value = null; version.value = ''; source.value = ''; baseline.value = ''; error.value = ''; notice.value = ''
  repairRun.value = ''; repairSamples.value = []
  if (!id) return
  busy.value = true
  try { const result = await call('authoring.get', { package_id: props.packageId, script_id: id }); if (token === scope && op === serial) accept(result.script, true) }
  catch (e) { if (token === scope && op === serial) error.value = e.message }
  finally { if (token === scope && op === serial) busy.value = false }
}
async function createScript() {
  if (!filename.value.trim() || busy.value) return
  if (!await beforeTabChange()) return
  const token = scope; busy.value = true; error.value = ''
  try { const result = await call('authoring.create', { package_id: props.packageId, script_id: filename.value.trim() }); if (token !== scope) return; selected.value = result.script.script_id; accept(result.script, true); creating.value = false; filename.value = ''; await refresh() }
  catch (e) { if (token === scope) error.value = e.message }
  finally { if (token === scope) busy.value = false }
}
async function importScript() {
  if (!importId.value || lifecycleBusy.value || !await beforeTabChange()) return
  const token = scope, pkg = props.packageId, id = importId.value
  busy.value = true; error.value = ''; notice.value = ''
  try {
    const result = await call('authoring.import', { package_id: pkg, script_id: id })
    if (token !== scope) return
    selected.value = result.script.script_id; accept(result.script, true)
    repairRun.value = ''; repairSamples.value = []; importing.value = false; importId.value = ''
    instruction.value = ''; notice.value = '已从脚本库导入工作稿，正式脚本和模板保留。'
    await refresh()
  } catch (e) { if (token === scope) error.value = e.message }
  finally { if (token === scope) busy.value = false }
}
async function deleteScript() {
  if (!doc.value || lifecycleBusy.value) return
  const token = scope, pkg = props.packageId, id = selected.value, head = doc.value.head_version || '', snapshot = source.value
  serial++; busy.value = true; error.value = ''; notice.value = ''
  try {
    const preview = await call('authoring.delete_preview', { package_id: pkg, script_id: id })
    if (token !== scope || id !== selected.value) return
    if (preview.busy) throw new Error('此脚本正在编辑或测试，请结束后再删除工作稿。')
    if ((preview.head_version || '') !== head) throw new Error('工作稿版本已变化，请刷新后再删除。')
    const message = `删除 ${id} 的 工作稿？将删除 ${preview.version_count || 0} 个私有版本、${preview.generated_template_count || 0} 个私有生成模板和 ${preview.private_run_count || 0} 次已结束私有试跑的日志及临时截图。${dirty.value ? '当前未保存修改也会丢弃。' : ''}脚本库中的正式文件、正式模板、发布记录和共享原素材均保留，手动保留的运行证据也会保留；后续可从脚本库重新导入修改。`
    const approved = await confirm(message, { title: '删除 工作稿', confirmText: '删除工作稿', danger: true })
    if (!approved || token !== scope || id !== selected.value || head !== (doc.value?.head_version || '') || source.value !== snapshot) return
    await call('authoring.delete', { package_id: pkg, script_id: id, expected_version: head, remove_private_runs: true })
    if (token !== scope || id !== selected.value) return
    serial++; refreshSerial++; selected.value = ''; doc.value = null; version.value = ''; source.value = ''; baseline.value = ''
    instruction.value = ''; repairRun.value = ''; repairSamples.value = []; chosenSamples.value = []; chosenTemplates.value = []
    testRun.value = null; testDevice.value = ''; testTarget.value = ''; testState.value = ''
    rememberAuthoringLocation(pkg, { scriptId: '' })
    notice.value = '工作稿及可清理的私有试跑数据已删除，脚本库、共享原素材和保留的运行证据未受影响。'
    await refresh()
  } catch (e) { if (token === scope && id === selected.value) error.value = e.message }
  finally { if (token === scope) busy.value = false }
}
async function operation(action, extra = {}) {
  if (busy.value || !selected.value) return
  const token = scope, id = selected.value; busy.value = true; error.value = ''; notice.value = ''
  try {
    const result = await call(action, { package_id: props.packageId, script_id: id, expected_version: doc.value?.head_version || '', version: version.value, ...extra })
    if (token !== scope || id !== selected.value) return
    if (result.script) accept(result.script, true)
    if (action === 'authoring.publish') { notifyYamlResourcesChanged(props.packageId, id); notice.value = `已发布 ${result.script.published_version}，脚本库和模板列表已更新`; await refresh() }
    if (action === 'authoring.save') notice.value = `已保存新版本 ${result.script.head_version}`
    if (action === 'authoring.validate') notice.value = result.diagnostics.length ? result.diagnostics.map(d => d.message).join('；') : result.scope === 'screenshot_templates' ? '语法、素材图片与模板匹配检查通过；脚本逻辑需测试当前版本' : '语法与引用检查通过；没有关联截图素材'
    if (result.run_id) { testRun.value = result.run_id; testDevice.value = extra.device_id; testTarget.value = `${props.packageId}/${id}`; testState.value = 'starting'; notice.value = `版本 ${version.value} 已提交测试：${result.run_id}`; showLogs() }
  } catch (e) { if (token === scope && id === selected.value) error.value = e.message }
  finally { if (token === scope && id === selected.value) busy.value = false }
}
async function attachFailure(requested) {
  const token = scope
  await selectScript(requested.scriptId)
  if (token !== scope || !doc.value || selected.value !== requested.scriptId) return
  busy.value = true
  try {
    const [run, trace] = await Promise.all([api.getRun(requested.runId), api.getRunTrace(requested.runId, 0, 1)])
    if (token !== scope) return
    if (run.state !== 'failed' || run.entrypoint !== `${props.packageId}/${selected.value}`) throw new Error('请选择当前脚本的失败运行')
    const frozen = trace.snapshot?._source_files?.[`automations/${selected.value}`]
    const revision = trace.snapshot?._script_revision
    const failedVersion = doc.value.versions.find(v => v.version === revision && v.yaml === frozen) || doc.value.versions.find(v => v.yaml === frozen)
    if (!failedVersion) throw new Error('未找到失败运行当时的脚本版本，不能用当前源码替代')
    version.value = failedVersion.version; baseline.value = failedVersion.yaml; source.value = baseline.value
    repairRun.value = requested.runId; repairSamples.value = failedVersion.material_refs || []
    notice.value = `已定位失败运行 ${requested.runId} 的原始版本。可查看判断图，修改后保存新版本。`
    request.repairRun = null
  } catch (e) { if (token === scope) error.value = e.message }
  finally { if (token === scope) busy.value = false }
}
async function openRequestedScript() {
  if (request.packageId !== props.packageId) return
  if (request.repairRun) { await attachFailure(request.repairRun); return }
  if (!request.importScriptId) { if (request.scriptId) await selectScript(request.scriptId); return }
  const token = scope, id = request.importScriptId
  try {
    const result = await call('authoring.list', { package_id: props.packageId })
    if (token !== scope || request.packageId !== props.packageId || request.importScriptId !== id) return
    if ((result.scripts || []).some(item => item.script_id === id)) await selectScript(id)
    else {
      importing.value = true; importId.value = id; creating.value = false
      notice.value = '此脚本尚无 工作稿，请从脚本库导入后继续修改。'
    }
    request.importScriptId = ''
  } catch (e) { if (token === scope) error.value = e.message }
}
async function changeVersion(next) { if (!await beforeTabChange()) return; version.value = next; baseline.value = doc.value?.versions.find(v => v.version === next)?.yaml || ''; source.value = baseline.value }
async function test() { try { const args = JSON.parse(testArgs.value); await operation('authoring.run', { device_id: workspace?.getSnapshot?.().deviceId, args }) } catch { error.value = '运行参数必须是 JSON 对象' } }
async function stopTest() { try { await api.cancelRun(testRun.value) } catch(e) { error.value = e.message } }
function showLogs() { emit('show-logs', { runId: testRun.value, deviceId: testDevice.value, target: testTarget.value }) }
async function poll() {
  clearTimeout(timer); if (disposed) return
  if (doc.value && running.value && !busy.value) {
    const token = scope, id = selected.value, op = serial
    try { const result = await call('authoring.get', { package_id: props.packageId, script_id: id }); if (token === scope && id === selected.value && op === serial && !busy.value) { const finished = result.script.job?.state !== 'generating'; accept(result.script, finished && !dirty.value); if (finished && result.script.job?.error) error.value = result.script.job.error; if (finished && result.script.job?.state === 'complete') { repairRun.value = ''; repairSamples.value = [] }; if (finished) await refresh() } }
    catch (e) { if (token === scope) error.value = e.message }
  }
  if (testRun.value && ['starting', 'running', 'paused', 'stopping'].includes(testState.value)) {
    const token = scope, id = testRun.value
    try { const record = await api.getRun(id); if (token === scope && testRun.value === id) { testState.value = record.state; if (record.finished_at) notice.value = record.state === 'success' ? '当前版本测试成功，请核对实际任务完成情况后发布。' : `当前版本测试${record.state}：${record.error || '请查看运行日志'}` } } catch(e) { if (token === scope) error.value = e.message }
  }
  if (!disposed) timer = setTimeout(poll, 1200)
}
watch(() => props.packageId, () => { confirm.cancel?.(); scope++; serial++; clearTimeout(timer); selected.value = ''; doc.value = null; source.value = ''; baseline.value = ''; instruction.value = ''; repairRun.value = ''; repairSamples.value = []; chosenSamples.value = []; chosenTemplates.value = []; importing.value = false; importId.value = ''; libraryScripts.value = []; error.value = ''; notice.value = ''; busy.value = false; testRun.value = null; testDevice.value = ''; testTarget.value = ''; testState.value = ''; void refresh(); timer = setTimeout(poll, 1200) }, { immediate: true, flush: 'sync' })
watch(() => yamlResourcesChanged.seq, () => { if (yamlResourcesChanged.packageId === props.packageId && !busy.value) void refresh() })
watch(() => request.seq, () => { void openRequestedScript() }, { immediate: true })
watch([dirty, busy], ([dirty, saving]) => emit('editing-state', { dirty, saving }), { immediate: true })
onBeforeUnmount(() => { disposed = true; scope++; serial++; clearTimeout(timer); emit('editing-state', { dirty: false, saving: false }) })
defineExpose({ beforeTabChange })
</script>
<template>
  <section class="authoring" aria-label="工作稿与版本">
    <p v-if="!packageId" class="hint">请先选择或新建配置包，再指定脚本和素材。</p>
    <template v-else>
      <div class="toolbar"><label>工作稿<select aria-label="指定脚本" :value="selected" :disabled="lifecycleBusy || loadingScripts" @change="selectScript($event.target.value)"><option value="">{{ loadingScripts ? '正在加载工作稿…' : '请选择工作稿' }}</option><option v-for="item in scripts" :key="item.script_id" :value="item.script_id">{{ item.script_id }}{{ item.published_version ? ` · 已发布 ${item.published_version}` : ' · 未发布' }}</option></select></label><button class="btn" :disabled="lifecycleBusy" @click="creating = !creating; importing = false">新建脚本</button><button class="btn" :disabled="lifecycleBusy" @click="importing = !importing; creating = false">从脚本库导入</button><button class="btn" :disabled="busy" @click="refresh">刷新</button><button v-if="doc" class="btn btn-danger" :disabled="lifecycleBusy" @click="deleteScript">删除工作稿</button></div>
      <p v-if="selected && busy && !doc" class="hint" role="status">正在读取脚本版本和源码…</p>
      <form v-if="creating" class="toolbar" @submit.prevent="createScript"><label>脚本名称<input v-model="filename" aria-label="新脚本名称" placeholder="例如：每日清理.yaml" /></label><button class="btn btn-primary" :disabled="busy || !filename.trim()">创建</button></form>
      <form v-if="importing" class="import-form" @submit.prevent="importScript"><div class="toolbar"><label>脚本库文件<select v-model="importId" aria-label="导入脚本库文件" :disabled="lifecycleBusy"><option value="">选择正式脚本</option><option v-for="item in libraryScripts" :key="item.id" :value="item.id.slice(packageId.length + 1)" :disabled="scripts.some(draft => draft.script_id === item.id.slice(packageId.length + 1))">{{ item.name || item.id.slice(packageId.length + 1) }}{{ scripts.some(draft => draft.script_id === item.id.slice(packageId.length + 1)) ? ' · 已有工作稿' : '' }}</option></select></label><button class="btn btn-primary" :disabled="lifecycleBusy || !importId">导入为工作稿</button></div><p class="hint">从正式脚本创建工作稿，后续修改需重新发布。<span v-if="!libraryScripts.length">当前配置包的脚本库为空。</span></p></form>
      <div v-if="!selected && !creating && !importing && !loadingScripts" class="authoring-empty"><h4>编辑工作稿与版本</h4><p>新建工作稿或从脚本库导入，手动编辑或通过自己的 Agent 修改源码。每次保存新增小版本，检查和测试后由你发布到脚本库。</p><p class="hint">外部 Agent 通过 MCP 访问获授权的脚本和素材；真实游戏效果在手动测试时确认。</p></div>
      <p v-if="error" role="alert" class="error">{{ error }}</p><p v-if="notice" role="status" class="notice">{{ notice }}</p>
      <template v-if="selected && doc">
        <div class="version-readiness" role="status"><strong>{{ current?.published ? '当前版本已发布' : current ? '当前是未发布版本' : '准备编辑第一个版本' }}</strong><span>{{ dirty ? '有未保存修改，请先保存。' : current?.published ? '可在脚本库选择并运行。' : '检查素材和实际运行效果后，再发布到脚本库。' }}</span></div>
        <details v-if="doc.job?.error"><summary>历史生成记录</summary><p class="hint">{{ doc.job.error }}</p></details>
        <details v-if="doc.job?.explanation"><summary>历史版本生成说明</summary><p class="hint">{{ doc.job.explanation }}</p></details>
        <label class="field">修改说明<textarea v-model="instruction" aria-label="版本修改说明" rows="5" :disabled="busy || running" placeholder="记录本次修改的目的和依据，随新版本保存。" /></label>
        <details><summary>查看素材与模板</summary><p class="hint">关联素材由版本记录保留。自己的 Agent 可通过 MCP 读取授权素材、裁切私有模板并保存新版本。</p><ul><li v-for="item in current?.material_refs || []" :key="`${item.plugin_id}:${item.sample_id}`">{{ item.plugin_id }} / {{ item.sample_id }}</li></ul><p v-if="loadingMaterials" class="hint" role="status">正在读取素材列表…</p><div class="material-list"><span v-for="item in samples" :key="item.id">{{ item.name || item.id }}</span><span v-for="item in templates" :key="item.name">{{ item.name }}</span></div><label class="field">导入素材包<input type="file" accept=".gamersample,.zip" :disabled="busy" @change="importSample" /></label></details>
        <p class="hint">需要 Agent 协助修改时，请在“设置 → MCP 与外部控制”创建接入令牌。每次保存只新增一个版本，发布由你确认。</p>
        <div class="toolbar"><label>版本<select aria-label="脚本版本" :value="version" :disabled="busy || running || !!repairRun" @change="changeVersion($event.target.value)"><option v-for="item in doc.versions.slice().reverse()" :key="item.version" :value="item.version">{{ item.version }}{{ item.published ? ' · 已发布' : ' · 小版本' }}</option></select></label><span v-if="doc.published_version" class="hint">脚本库使用 {{ doc.published_version }}</span></div>
        <details class="authoring-source" open><summary>查看或手动编辑源码</summary><textarea class="source" v-model="source" aria-label="脚本 YAML" spellcheck="false" :disabled="running || busy" placeholder="在此编辑 YAML，保存会新增小版本；已有版本保持可查看。" /></details>
        <p v-if="dirty" class="hint">有未保存修改，保存后会新增小版本。</p><ul v-if="diagnostics.length" class="error"><li v-for="(item,index) in diagnostics" :key="index">{{ item.path }} {{ item.message }}</li></ul>
        <div class="toolbar"><button class="btn" :disabled="!dirty || running || busy" @click="operation('authoring.save', { yaml: source, instruction })">保存修改为小版本</button><button class="btn" :disabled="!current || dirty || running || busy" @click="operation('authoring.validate')">检查语法与素材</button><button class="btn" :disabled="!current || dirty || running || busy || !workspace?.getSnapshot?.().deviceId" @click="test">测试当前版本</button><button class="btn btn-primary" :disabled="!current || dirty || diagnostics.length || running || busy || current.published" @click="operation('authoring.publish')">发布到脚本库</button></div>
        <details v-if="current"><summary>版本记录与用量</summary><p class="hint">{{ current.created_at }} · {{ current.source }} · {{ current.instruction }}</p><pre v-if="current.usage">{{ JSON.stringify(current.usage, null, 2) }}</pre><p class="hint">版本号保存在脚本记录中，不写入 YAML。</p></details>
        <details v-if="dirty"><summary>当前修改</summary><pre>{{ sourceDiff(current?.yaml || '', source) }}</pre></details>
        <details><summary>测试参数</summary><textarea v-model="testArgs" aria-label="版本测试参数" rows="2" /><p class="hint">JSON 对象；仅手动点击测试时执行，源码编辑和素材检查不会控制设备。</p></details>
        <div v-if="testRun" class="toolbar"><button class="btn" @click="showLogs">查看运行日志</button><button v-if="['starting','running','paused'].includes(testState)" class="btn btn-danger" @click="stopTest">停止测试</button><span>{{ testState }}</span></div>
      </template>
    </template>
  </section>
</template>
<style scoped>
.authoring-empty{display:flex;flex-direction:column;gap:12px;padding:16px 0}.authoring-empty h4{font-size:17px}.authoring-empty>p{font-size:14px;line-height:1.7;color:var(--text-2)}.version-readiness{display:flex;flex-direction:column;gap:5px;font-size:13px;padding:6px 0}.version-readiness span{color:var(--text-2);line-height:1.6}.authoring-source summary{min-height:32px;padding:6px 0;color:var(--text-1)}.authoring-source .source{margin-top:8px}
.authoring{display:flex;flex:1;min-height:0;overflow:auto;flex-direction:column;gap:12px;padding:8px;font-size:12px}.toolbar{display:flex;align-items:center;gap:8px;flex-wrap:wrap}.toolbar label{display:flex;align-items:center;gap:8px;flex:1;min-width:180px}.toolbar select{flex:1;min-width:0}.field{display:flex;flex-direction:column;gap:6px}textarea,input,select{border:1px solid var(--border);border-radius:4px;background:var(--bg-0);color:var(--text-0);padding:8px;box-sizing:border-box}textarea{width:100%;line-height:1.6;resize:vertical}.source{min-height:260px;font-family:var(--mono,monospace);font-size:13px;tab-size:2}.material-list{display:flex;flex-wrap:wrap;gap:8px 18px;max-height:180px;overflow:auto;padding:8px 0}.material-list label{display:flex;align-items:center;gap:5px;overflow-wrap:anywhere}.hint{color:var(--text-2);line-height:1.7}.error{color:var(--danger);line-height:1.6;overflow-wrap:anywhere}.notice{color:var(--ok);line-height:1.7}summary{cursor:pointer;line-height:1.8}pre{white-space:pre-wrap;overflow-wrap:anywhere;font-family:var(--mono,monospace);max-height:240px;overflow:auto}:focus-visible{outline:2px solid var(--accent);outline-offset:2px}@media(max-width:640px){.toolbar label{flex-basis:100%}.toolbar .btn{flex:1}.source{min-height:220px}}
</style>
