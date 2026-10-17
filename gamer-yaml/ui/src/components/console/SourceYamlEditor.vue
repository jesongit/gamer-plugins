<script setup>
import { computed, inject, onBeforeUnmount, reactive, ref, watch } from 'vue'
import { load as parseYaml } from 'js-yaml'
import { api } from '../../../../../../web/src/api'
import { scriptsData, applyRunRecord } from '../../../../../../web/src/store'
import { WORKSPACE_CONTEXT_KEY } from '../../../../../../web/src/workspace/context'
import { useConfirmDialog } from '../../../../../../web/src/components/ui/useConfirmDialog'
import { useSourceYamlEditor } from '../../composables/useSourceYamlEditor'
import { addScriptFunction, scriptCallSnippet } from './script-functions'
import { pluginMessageChannel } from '../../../../../../web/src/workspace/plugin-messages'
import { yamlResourcesChanged } from './resource-changes'
const props = defineProps({ context: { type: Object, required: true } })
const emit = defineEmits(['show-logs', 'generate'])
const ctx = reactive(props.context), workspace = inject(WORKSPACE_CONTEXT_KEY, null), confirm = useConfirmDialog()
const selected = ref(''), resources = ref([]), functionName = ref('')
const editor = reactive(useSourceYamlEditor({ api, call: (action, values) => api.callExtension('gamer-yaml', action, values) }))
const busy = computed(() => editor.loading || editor.saving || ctx.startPending || ctx.store.running)
const functionNames = computed(() => { try { return Object.keys(parseYaml(editor.content)?.functions || {}) } catch { return [] } })
const newFunctionName = ref(''), referenceTarget = ref('')
const referenceOptions = ref([])
const draftVersion = ref(''), saveNotice = ref('')
const sourceOpen = ref(false)
watch(() => editor.dirty, dirty => { if (dirty) sourceOpen.value = true })
async function openPackages() {
  try { await workspace?.uiBridge.workspace.openPanel('packages') }
  catch (e) { editor.error = e.message }
}
let scope = 0, fetching = 0
async function refresh() {
  const token = scope, pkg = ctx.packageId, request = ++fetching
  if (!pkg) return
  try {
    const list = await api.listScripts(pkg)
    if (token !== scope || request !== fetching) return
    resources.value = list || []
    referenceOptions.value = resources.value.map(item => ({ value: item.id.slice(pkg.length + 1), label: item.name || item.id.slice(pkg.length + 1) }))
    scriptsData.value = resources.value
    // Offline authoring retains its first-file convenience; an execution target must opt in.
    if (!ctx.store.deviceId && !selected.value && resources.value.length && !editor.dirty) {
      selected.value = resources.value[0].id
      await editor.load(selected.value)
      if (token === scope) ctx.selScript = selected.value
    }
  } catch (e) { if (token === scope && request === fetching) editor.error = e.message }
}
async function beforeTabChange() {
  const token = scope, source = editor.content
  if (editor.saving || editor.loading) return false
  if (editor.dirty && !await confirm('原文有未保存修改，放弃后将切换内容。', { title: '放弃修改', confirmText: '放弃并切换', danger: true })) return false
  if (token !== scope || source !== editor.content || editor.saving || editor.loading) return false
  editor.reset(ctx.packageId, 'script')
  draftVersion.value = ''; saveNotice.value = ''
  return true
}
async function selectResource(event) {
  const id = event.target.value, token = scope
  if (await beforeTabChange() && token === scope) {
    selected.value = id
    if (id) await editor.load(id)
    if (token === scope) ctx.selScript = id
  }
  event.target.value = selected.value
}
async function create() {
  if (!await beforeTabChange()) return
  selected.value = ''
  editor.create('automation.yaml', '# 按顺序编写步骤；全部步骤执行完成即成功，步骤报错即失败。\nrun:\n  - log: 开始执行\n')
}
async function save() {
  const token = scope
  {
    const script_id = editor.name.trim(), pkg = ctx.packageId
    if (!script_id || !pkg || editor.saving) return false
    editor.saving = true; editor.error = ''
    try {
      const snapshot = editor.content
      let latest
      try { latest = await api.callExtension('gamer-yaml', 'authoring.get', { package_id: pkg, script_id }) }
      catch (e) {
        if (!String(e.message || '').includes('authoring.draft_not_found') && e.data?.code !== 'authoring.draft_not_found') throw e
        if (token !== scope) return false
        latest = await api.callExtension('gamer-yaml', editor.id ? 'authoring.import' : 'authoring.create', { package_id: pkg, script_id })
      }
      if (token !== scope) return false
      const result = await api.callExtension('gamer-yaml', 'authoring.save', { package_id: pkg, script_id, expected_version: latest.script.head_version, yaml: snapshot })
      if (token !== scope) return false
      draftVersion.value = result.script.head_version; editor.id = `${pkg}/${script_id}`; editor.savedContent = snapshot
      editor.diagnostics = result.script.versions.at(-1).diagnostics; editor.valid = !editor.diagnostics.length
      saveNotice.value = `已保存小版本 ${draftVersion.value}；发布请进入 工作稿的版本管理`
      return true
    } catch (e) { if (token === scope) editor.error = e.message; return false }
    finally { if (token === scope) editor.saving = false }
  }

}
async function run() {
  const token = scope
  if (busy.value || !editor.id && !editor.name) return
  if (editor.dirty && !await save()) return
  if (token !== scope) return
  if (draftVersion.value) {
    try {
      const run = await api.callExtension('gamer-yaml', 'authoring.run', { package_id: ctx.packageId, script_id: editor.name, expected_version: draftVersion.value, version: draftVersion.value, device_id: ctx.store.deviceId })
      applyRunRecord({ ...run, state: 'starting', device_id: ctx.store.deviceId, runner_id: 'gamer-yaml-version', entrypoint: editor.id })
      ctx.startRunStatusPoll?.()
    }
    catch(e) { editor.error = e.message }
  } else { ctx.selScript = editor.id; try { await ctx.runScript({ startIndex: 0 }) } catch(e) { editor.error = e.message } }
}
async function reload() { if (await beforeTabChange()) await editor.load(selected.value) }
function manageVersions() {
  const request = pluginMessageChannel('gamer-yaml:open-generation')
  request.packageId = ctx.packageId; request.scriptId = editor.name; request.importScriptId = draftVersion.value ? '' : editor.name; request.candidateId = null; request.seq++
}
watch([() => ctx.packageId, () => ctx.store.deviceId], () => {
  const restore = ctx.sourcePackage === ctx.packageId ? ctx.sourceFile || ctx.selScript : ctx.selScript
  const restoreFunction = ctx.sourcePackage === ctx.packageId ? ctx.sourceFunction : ''
  scope++; selected.value = restore && restore.startsWith(`${ctx.packageId}/`) ? restore : ''; resources.value = []; functionName.value = restoreFunction || ''
  draftVersion.value = ''; saveNotice.value = ''
  ctx.sourcePackage = ctx.packageId
  editor.reset(ctx.packageId, 'script'); if (selected.value) void editor.load(selected.value); void refresh()
}, { immediate: true })
watch(functionNames, names => { if (!names.includes(functionName.value)) functionName.value = names.includes(ctx.sourceFunction) ? ctx.sourceFunction : names[0] || '' })
watch([selected, functionName], () => { ctx.sourceFile = selected.value; if (functionName.value) ctx.sourceFunction = functionName.value }, { flush: 'sync' })
watch(() => ctx.selScript, async id => {
  if (!id || id === editor.id || editor.dirty || !id.startsWith(`${ctx.packageId}/`)) return
  selected.value = id; await editor.load(id)
  draftVersion.value = ''; saveNotice.value = ''
})
watch(() => yamlResourcesChanged.seq, async () => {
  if (yamlResourcesChanged.packageId !== ctx.packageId) return
  const token = scope, id = selected.value
  await refresh()
  if (token !== scope || id !== selected.value || editor.dirty || busy.value || draftVersion.value) return
  if (id && id === `${ctx.packageId}/${yamlResourcesChanged.scriptId}`) await editor.load(id)
})
function showLogs() { emit('show-logs', { runId: ctx.store.runId, deviceId: ctx.store.deviceId, target: editor.id }) }
watch(() => ctx.store.runId, id => { if (id) showLogs() })
// Share only guard state with the existing package-navigation guard. Raw source never enters its old form codec.
watch([() => editor.dirty, () => editor.saving], () => { ctx.sourceEditorDirty = editor.dirty; ctx.sourceEditorSaving = editor.saving }, { immediate: true, flush: 'sync' })
function beforeUnload(event) { if (editor.dirty) { event.preventDefault(); event.returnValue = '' } }
window.addEventListener('beforeunload', beforeUnload)
onBeforeUnmount(() => { scope++; editor.reset(); ctx.sourceEditorDirty = false; ctx.sourceEditorSaving = false; window.removeEventListener('beforeunload', beforeUnload) })
function addFunction() {
  try {
    editor.content = addScriptFunction(editor.content, newFunctionName.value.trim())
    editor.edited(); newFunctionName.value = ''
  } catch (e) { editor.error = e.message }
}
async function chooseReference(event) {
  referenceTarget.value = event.target.value
  if (!referenceTarget.value) return
  const token = scope
  try {
    const file = referenceTarget.value.split('#')[0]
    const resource = await api.getScript(`${ctx.packageId}/${file}`)
    if (token !== scope) return
    const functions = Object.keys(parseYaml(resource.content)?.functions || {})
    referenceOptions.value = referenceOptions.value.filter(item => !item.value.startsWith(`${file}#`))
    referenceOptions.value.push(...functions.map(name => ({ value: `${file}#${name}`, label: `${file} · ${name}` })))
  } catch (e) { if (token === scope) editor.error = e.message }
}
async function copyReference() {
  const [file, name] = referenceTarget.value.split('#')
  try { await navigator.clipboard.writeText(scriptCallSnippet(file, name)); saveNotice.value = '调用步骤已复制，请粘贴到 run 或函数的 run 中' }
  catch { editor.error = '无法访问剪贴板，请按下面的调用示例复制' }
}
defineExpose({ beforeTabChange })
</script>
<template>
  <section class="source-workspace" aria-label="YAML 源码编辑器">
    <p v-if="saveNotice" class="hint" role="status">{{ saveNotice }} <button class="btn btn-sm" @click="manageVersions">版本管理</button></p>
    <div class="toolbar">
      <select class="select" :value="selected" :disabled="busy" aria-label="选择源码文件" @change="selectResource"><option value="">选择脚本</option><option v-for="resource in resources" :key="resource.id" :value="resource.id">{{ resource.name || resource.file || resource.id }}</option></select>
      <button v-if="resources.length || editor.name" class="btn" :disabled="busy || !ctx.packageId" @click="create">手动编写</button>
      <button v-if="editor.dirty && editor.name" class="btn" :disabled="busy" @click="save" title="保存为未发布版本，不替换脚本库">保存草稿</button>
      <button v-if="ctx.configureTargetScript && editor.id" class="btn" :disabled="busy || editor.dirty || !ctx.store.deviceId || ctx.runArgsFlow?.modal.loading" @click="ctx.configureTargetScript">运行设置</button>
      <button v-if="!ctx.store.running && editor.name" class="btn btn-primary" :disabled="busy || !ctx.store.deviceId" @click="run" :title="editor.dirty ? '先保存修改，再在当前目标上运行一次' : '在当前游戏目标上运行一次'">{{ editor.dirty ? '保存并运行' : '运行脚本' }}</button>
      <button v-if="ctx.store.running" class="btn btn-danger" :disabled="ctx.runStopping" @click="ctx.stopScript">停止脚本</button>
      <button v-if="editor.name || ctx.store.runId" class="btn" @click="showLogs">运行日志</button>
    </div>
    <div v-if="editor.name" class="script-status" role="status">
      <span>{{ editor.dirty ? '有未保存修改' : draftVersion ? '已保存草稿，尚未发布' : '脚本已读取' }}</span>
      <button class="btn btn-sm" :disabled="busy || editor.dirty || !editor.id" @click="manageVersions" title="先保存当前修改，再进入版本管理">修改与版本管理</button>
    </div>
    <p v-if="editor.name && !ctx.store.deviceId" class="hint">选择游戏目标后可运行；编辑和源码校验可离线进行。</p>
    <p v-if="editor.valid" class="success" role="status">源码校验通过；实际目标与素材验证仍需单独执行</p>
    <p v-if="editor.error" role="alert" class="error">{{ editor.error }} <button v-if="editor.conflict" class="btn" @click="reload">放弃修改并重载最新版本</button></p>
    <ul v-if="editor.diagnostics.length" class="diagnostics"><li v-for="(d,index) in editor.diagnostics" :key="index">{{ d.path || d.step_path || d.field || '文档' }} · {{ d.message }} ({{ d.code }})</li></ul>
    <details v-if="editor.name" class="source-disclosure" :open="sourceOpen" @toggle="sourceOpen = $event.currentTarget.open">
      <summary>查看和编辑源码</summary>
      <div class="source-editing">
      <div class="toolbar"><label class="source-filename">脚本名称<input v-model="editor.name" class="input" aria-label="源码文件名" :disabled="!!editor.id || busy" /></label><button class="btn" :disabled="busy || editor.validating" @click="editor.validate">{{ editor.validating ? '校验中…' : '校验源码' }}</button></div>
      <details class="source-reuse"><summary>函数与脚本复用</summary>
      <div class="toolbar">
        <input v-model="newFunctionName" class="input" aria-label="新函数名" placeholder="函数名" :disabled="busy" @keydown.enter.prevent="addFunction" />
        <button class="btn" :disabled="busy || !newFunctionName.trim()" @click="addFunction">新增函数</button>
      </div>
      <p v-if="functionNames.length" class="hint">当前脚本函数：{{ functionNames.join('、') }}</p>
      <details><summary>引用其他脚本或函数</summary>
        <div class="toolbar"><select class="select" aria-label="引用脚本或函数" :value="referenceTarget" :disabled="busy" @change="chooseReference"><option value="">选择同配置包脚本</option><option v-for="item in referenceOptions" :key="item.value" :value="item.value">{{ item.label }}</option></select><button class="btn" :disabled="!referenceTarget" @click="copyReference">复制调用步骤</button></div>
        <pre v-if="referenceTarget">{{ scriptCallSnippet(...referenceTarget.split('#')) }}</pre>
        <p class="hint">选脚本后会列出其中的函数。调用步骤可放进 run 或函数的 run；本脚本函数直接按函数名调用。</p>
      </details>
      </details>
      <p class="hint">编辑 YAML 原文会保留完整语法。保存草稿后，到版本管理检查、测试并发布。</p>
      <textarea v-model="editor.content" class="source" aria-label="YAML 原文" spellcheck="false" :disabled="busy" @input="editor.edited" />
      </div>
    </details>
    <div v-else class="library-empty">
      <h4>{{ resources.length ? '选择脚本，开始日常操作' : '当前配置包还没有脚本' }}</h4>
      <p>{{ resources.length ? '在上方选择脚本，设置参数后即可运行。需要修改时可展开源码，或通过自己的 Agent 使用 MCP 编辑工作稿。' : '导入已有配置包即可使用；也可以手动编写或通过自己的 Agent 制作脚本。' }}</p>
      <div class="empty-actions"><button class="btn btn-primary" :disabled="busy || !ctx.packageId" @click="emit('generate')">创建工作稿</button><button class="btn" @click="openPackages">导入配置包</button><button class="btn btn-ghost" :disabled="busy || !ctx.packageId" @click="create">手动编写</button></div>
    </div>
  </section>
</template>
<style scoped>
.source-workspace{min-width:0;overflow:auto;padding:2px;box-sizing:border-box}
.script-status,.empty-actions{display:flex;align-items:center;gap:8px;flex-wrap:wrap}.script-status{justify-content:space-between;font-size:13px;color:var(--text-2)}.source-disclosure{min-height:0}.source-disclosure>summary,.source-reuse>summary{cursor:pointer;min-height:32px;padding:6px 0;font-size:13px;color:var(--text-1)}.source-editing{display:flex;flex-direction:column;gap:8px;padding:2px}.source-filename{display:flex;align-items:center;gap:8px;flex:1;min-width:180px;font-size:13px}.source-filename .input{min-width:0}.source-reuse{padding:4px 0}.library-empty{display:flex;flex-direction:column;gap:12px;padding:20px 2px}.library-empty h4{font-size:17px}.library-empty p{font-size:14px;line-height:1.7;color:var(--text-2)}
.source-workspace{display:flex;flex:1;min-height:0;flex-direction:column;gap:8px}.toolbar{display:flex;gap:6px;flex-wrap:wrap}.toolbar .select,.toolbar .input{flex:1;min-width:120px}.source{flex:1;min-height:260px;resize:vertical;font-family:var(--mono,monospace);font-size:13px;line-height:1.6;padding:12px;background:var(--bg-0);color:var(--text-0);border:1px solid var(--border);border-radius:4px;tab-size:2;box-sizing:border-box}.source:focus{outline:2px solid var(--accent)}.hint,.empty{font-size:12px;color:var(--text-2);line-height:1.6}.error,.diagnostics{font-size:12px;color:var(--danger);white-space:pre-wrap;overflow-wrap:anywhere}.diagnostics{max-height:150px;overflow:auto}.success{font-size:12px;color:var(--ok)}
</style>
