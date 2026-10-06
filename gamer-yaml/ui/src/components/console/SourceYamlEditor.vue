<script setup>
import { computed, inject, onBeforeUnmount, reactive, ref, watch } from 'vue'
import { load as parseYaml } from 'js-yaml'
import { api } from '../../../../../../web/src/api'
import { scriptsData } from '../../../../../../web/src/store'
import { WORKSPACE_CONTEXT_KEY } from '../../../../../../web/src/workspace/context'
import { useConfirmDialog } from '../../../../../../web/src/components/ui/useConfirmDialog'
import { useSourceYamlEditor } from '../../composables/useSourceYamlEditor'
import RunDetails from './RunDetails.vue'
import RunErrorLocation from './RunErrorLocation.vue'
import { requestAutomationContext } from './automationAiBridge'
const props = defineProps({ context: { type: Object, required: true } })
const ctx = reactive(props.context), workspace = inject(WORKSPACE_CONTEXT_KEY, null), confirm = useConfirmDialog()
const isFunction = computed(() => ctx.runKind === 'func'), selected = ref(''), resources = ref([]), showLogs = ref(false), runLocation = ref(null), functionName = ref('')
const editor = reactive(useSourceYamlEditor({ api, call: (action, values) => api.callExtension('gamer-yaml', action, values) }))
const busy = computed(() => editor.loading || editor.saving || ctx.startPending || ctx.store.running)
const functionNames = computed(() => { try { return Object.keys(parseYaml(editor.content)?.functions || {}) } catch { return [] } })
const target = computed(() => isFunction.value ? `${ctx.packageId}#${functionName.value}` : editor.id)
const aiAvailable = ref(false)
let scope = 0
async function checkAi() { const token = scope; try { const list = await api.listExtensions(); if (token === scope) aiAvailable.value = (Array.isArray(list) ? list : list.extensions || []).some(item => item.id === 'gamer-ai' && item.state === 'running') } catch { if (token === scope) aiAvailable.value = false } }
async function refresh() {
  const token = scope, pkg = ctx.packageId
  if (!pkg) return
  try {
    const list = await (isFunction.value ? api.listFunctions(pkg) : api.listScripts(pkg))
    if (token !== scope) return
    resources.value = list || []
    if (!isFunction.value) scriptsData.value = resources.value
    if (!selected.value && resources.value.length) { selected.value = resources.value[0].id; await editor.load(selected.value); if (!isFunction.value) ctx.selScript = selected.value }
  } catch (e) { if (token === scope) editor.error = e.message }
}
async function beforeTabChange() {
  const token = scope, source = editor.content
  if (editor.saving || editor.loading) return false
  if (editor.dirty && !await confirm('原文有未保存修改，放弃后将切换内容。', { title: '放弃修改', confirmText: '放弃并切换', danger: true })) return false
  if (token !== scope || source !== editor.content || editor.saving || editor.loading) return false
  editor.reset(ctx.packageId, isFunction.value ? 'function_library' : 'script')
  return true
}
async function selectResource(event) {
  const id = event.target.value, token = scope
  if (await beforeTabChange() && token === scope) {
    selected.value = id
    if (id) await editor.load(id)
    if (token === scope && !isFunction.value) ctx.selScript = id
  }
  event.target.value = selected.value
}
async function create() {
  if (!await beforeTabChange()) return
  selected.value = ''
  editor.create(isFunction.value ? '_function.yaml' : 'automation.yaml', isFunction.value
    ? 'version: 2\nfunctions:\n  example:\n    run:\n      - log: "hello"\n'
    : 'version: 2\ntargets:\n  done:\n    template: done.png\nrun:\n  - finish: done\n    timeout: 10s\n')
  showLogs.value = false
}
async function save() {
  const token = scope
  if (!await editor.save() || token !== scope) return false
  selected.value = editor.id
  if (!isFunction.value) ctx.selScript = editor.id
  await refresh(); return token === scope
}
async function run() {
  const token = scope
  if (busy.value || !editor.id && !editor.name) return
  if (editor.dirty && !await save()) return
  if (token !== scope) return
  if (isFunction.value) await ctx.runFunction({ fnName: functionName.value })
  else { ctx.selScript = editor.id; await ctx.runScript({ startIndex: 0 }) }
}
async function reload() { if (await beforeTabChange()) await editor.load(selected.value) }
async function sendToAi() {
  if (!editor.id) return
  requestAutomationContext(ctx.packageId, { script_id: editor.id.slice(ctx.packageId.length + 1) })
  try { await workspace?.uiBridge.workspace.openPanel('gamer-ai:ai') } catch (e) { editor.error = e.message }
}
watch(() => ctx.packageId, () => {
  const restore = ctx.sourcePackage === ctx.packageId ? ctx.sourceFile || (!isFunction.value ? ctx.selScript : '') : (!isFunction.value ? ctx.selScript : '')
  const restoreFunction = ctx.sourcePackage === ctx.packageId ? ctx.sourceFunction : ''
  scope++; selected.value = restore && restore.startsWith(`${ctx.packageId}/`) ? restore : ''; resources.value = []; functionName.value = restoreFunction || ''; showLogs.value = false
  ctx.sourcePackage = ctx.packageId
  editor.reset(ctx.packageId, isFunction.value ? 'function_library' : 'script'); if (selected.value) void editor.load(selected.value); void refresh(); void checkAi()
}, { immediate: true, flush: 'sync' })
watch(functionNames, names => { if (!names.includes(functionName.value)) functionName.value = names.includes(ctx.sourceFunction) ? ctx.sourceFunction : names[0] || '' })
watch([selected, functionName], () => { ctx.sourceFile = selected.value; if (functionName.value) ctx.sourceFunction = functionName.value }, { flush: 'sync' })
watch(() => ctx.selScript, async id => {
  if (isFunction.value || !id || id === editor.id || editor.dirty || !id.startsWith(`${ctx.packageId}/`)) return
  selected.value = id; await editor.load(id)
})
watch(() => ctx.store.runId, id => { if (id) showLogs.value = true })
// Share only guard state with the existing package-navigation guard. Raw source never enters its old form codec.
watch([() => editor.dirty, () => editor.saving], () => { ctx.sourceEditorDirty = editor.dirty; ctx.sourceEditorSaving = editor.saving }, { immediate: true, flush: 'sync' })
function beforeUnload(event) { if (editor.dirty) { event.preventDefault(); event.returnValue = '' } }
window.addEventListener('beforeunload', beforeUnload)
onBeforeUnmount(() => { scope++; editor.reset(); ctx.sourceEditorDirty = false; ctx.sourceEditorSaving = false; window.removeEventListener('beforeunload', beforeUnload) })
defineExpose({ beforeTabChange })
</script>
<template>
  <section class="source-workspace" aria-label="YAML 源码编辑器">
    <div class="toolbar">
      <select class="select" :value="selected" :disabled="busy" aria-label="选择源码文件" @change="selectResource"><option value="">选择{{ isFunction ? '函数库' : '脚本' }}</option><option v-for="resource in resources" :key="resource.id" :value="resource.id">{{ resource.name || resource.file || resource.id }}</option></select>
      <button class="btn" :disabled="busy || !ctx.packageId" @click="create">新建</button>
      <button class="btn" :disabled="busy || !editor.dirty || !editor.name" @click="save">保存</button>
      <button v-if="!ctx.store.running" class="btn btn-primary" :disabled="busy || !ctx.store.deviceId || !editor.name || isFunction && !functionName" @click="run">运行</button>
      <button v-else class="btn btn-danger" :disabled="ctx.runStopping" @click="ctx.stopScript">停止</button>
      <button class="btn" @click="showLogs = !showLogs">{{ showLogs ? '原文' : '日志' }}</button>
    </div>
    <RunDetails v-if="showLogs" :device-id="ctx.store.deviceId" :target="target" :live-run="ctx.store.runId" @edit="showLogs = false" @locate="runLocation = $event" />
    <template v-else>
      <div v-if="editor.name" class="toolbar"><input v-model="editor.name" class="input" aria-label="源码文件名" :disabled="!!editor.id || busy" /><select v-if="isFunction" v-model="functionName" class="select" aria-label="运行函数"><option v-for="name in functionNames" :key="name">{{ name }}</option></select><button class="btn" :disabled="busy || editor.validating" @click="editor.validate">{{ editor.validating ? '校验中…' : '校验源码' }}</button><button class="btn" :disabled="!editor.id || editor.dirty || busy || isFunction || !aiAvailable" :title="aiAvailable ? '附加当前已保存脚本' : 'AI 助手未运行'" @click="sendToAi">交给 AI 分析</button></div>
      <p class="hint">YAML v2 原文编辑，保留注释与未知字段。{{ isFunction ? '按完整函数库保存，运行时选择函数。' : 'wait 只观察；finish 必须确认完成画面。' }} {{ editor.dirty ? '未保存' : editor.id ? '已保存' : '' }}</p>
      <p v-if="editor.valid" class="success" role="status">源码校验通过；实际目标与素材验证仍需单独执行</p>
      <p v-if="editor.error" role="alert" class="error">{{ editor.error }} <button v-if="editor.conflict" class="btn" @click="reload">放弃修改并重载最新版本</button></p>
      <ul v-if="editor.diagnostics.length" class="diagnostics"><li v-for="(d,index) in editor.diagnostics" :key="index">{{ d.path || d.step_path || d.field || '文档' }} · {{ d.message }} ({{ d.code }})</li></ul>
      <textarea v-if="editor.name" v-model="editor.content" class="source" aria-label="YAML 原文" spellcheck="false" :disabled="busy" @input="editor.edited" />
      <p v-else class="empty">选择文件或新建自动化，无需连接设备即可编辑和校验</p>
    </template>
    <RunErrorLocation v-if="runLocation" :event="runLocation" @close="runLocation = null" />
  </section>
</template>
<style scoped>
.source-workspace{display:flex;flex:1;min-height:0;flex-direction:column;gap:8px}.toolbar{display:flex;gap:6px;flex-wrap:wrap}.toolbar .select,.toolbar .input{flex:1;min-width:120px}.source{flex:1;min-height:260px;resize:vertical;font-family:var(--mono,monospace);font-size:13px;line-height:1.6;padding:12px;background:var(--bg-0);color:var(--text-0);border:1px solid var(--border);border-radius:4px;tab-size:2;box-sizing:border-box}.source:focus{outline:2px solid var(--accent)}.hint,.empty{font-size:12px;color:var(--text-2);line-height:1.6}.error,.diagnostics{font-size:12px;color:var(--danger);white-space:pre-wrap;overflow-wrap:anywhere}.diagnostics{max-height:150px;overflow:auto}.success{font-size:12px;color:var(--ok)}
</style>
