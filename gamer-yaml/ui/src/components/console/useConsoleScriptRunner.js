import { computed, onUnmounted, reactive, ref, watch } from 'vue'
import { api } from '../../../../../../web/src/api'
import { packageStore, selectPackage } from '../../../../../../web/src/package-store'
import { store, scriptsData, applyRunRecord, beginCancel, pushRunConflict } from '../../../../../../web/src/store'
import { useConfirmDialog } from '../../../../../../web/src/components/ui/useConfirmDialog'
import { useRunArgsFlow } from '../../composables/useRunArgsFlow'
import { GAMER_YAML_RUNNER_ID, runYamlScript, runYamlFunction } from '../../gamer-yaml-runner'
import { automationEditorRequest } from './automationEditorBridge'
import { yamlResourcesChanged } from './resource-changes'

/** Raw source editing belongs to SourceYamlEditor. This hook only owns target
 * selection, server-described arguments, input guards and run status. */
export function useConsoleScriptRunner({ toast, packageId, restorePackage = previous => { packageId.value = previous }, multiviewWorkspace = null, consoleRuntime = {} }) {
  const confirm = useConfirmDialog(), selScript = ref(''), automationTab = ref('scripts')
  const sourceEditorDirty = ref(false), sourceEditorSaving = ref(false), startPending = ref(false), runStopping = ref(false)
  const liveLogs = ref([]), resourcePreview = reactive({ open: false })
  let disposed = false, fetching = 0, polling, restoring = false, projecting = false
  async function refreshScripts() { const token = ++fetching, pkg = packageId.value; const rows = pkg ? await api.listScripts(pkg) : []; if (!disposed && token === fetching && pkg === packageId.value) scriptsData.value = rows || []; return rows }
  const scripts = computed(() => scriptsData.value.filter(s => s.pkg === packageId.value || s.id?.startsWith(`${packageId.value}/`)))
  function pushLog(level, msg) { liveLogs.value.push({ level, msg, time: new Date().toLocaleTimeString() }); if (liveLogs.value.length > 50) liveLogs.value.shift() }
  async function exec(opts) {
    if (!store.deviceId || startPending.value) return
    startPending.value = true
    const device = store.deviceId
    try {
      const rep = opts.kind === 'function_library' ? await runYamlFunction(packageId.value, device, { function: opts.fnName, args: opts.args }) : await runYamlScript(opts.id, device, 0, opts.args)
      applyRunRecord({ ...rep, device_id: device, runner_id: GAMER_YAML_RUNNER_ID, entrypoint: opts.kind === 'function_library' ? `${packageId.value}#${opts.fnName}` : opts.id })
      pushLog('info', `运行已提交：${rep.run_id}`); startRunStatusPoll(); return rep
    } catch (e) { if (e.status === 409) pushRunConflict(e.data); toast?.(e.message, 'error'); throw e }
    finally { startPending.value = false }
  }
  const runArgsFlow = useRunArgsFlow({ exec, saveConfiguration: opts => {
    if (opts.deviceId !== store.deviceId || opts.id !== selScript.value) throw new Error('运行目标已变化，请重新配置')
    return multiviewWorkspace?.updateConfig(opts.deviceId, { args: opts.args || {} })
  }, notify: () => {} })
  function configureTargetScript() {
    const deviceId=store.deviceId, id=selScript.value
    if(!deviceId || !id)return
    return runArgsFlow.begin({mode:'configure',title:'运行配置',submitLabel:'保存配置',id,name:id,kind:'script',runnerId:GAMER_YAML_RUNNER_ID,entrypoint:id,deviceId,startIndex:0,initialArgs:multiviewWorkspace?.getConfig(deviceId)?.args || {},useConfiguredArgs:true})
  }
  async function runScript(opts = {}) {
    const id = selScript.value
    if (!id || !store.deviceId) return toast?.('请先选择脚本和设备', 'warn')
    return runArgsFlow.begin({ id, name: id, kind: 'script', runnerId: GAMER_YAML_RUNNER_ID, entrypoint: id, deviceId: store.deviceId, startIndex: 0, initialArgs: multiviewWorkspace?.getConfig(store.deviceId)?.args || {}, useConfiguredArgs: true, ...opts })
  }
  async function runFunction(opts = {}) {
    if (!opts.fnName || !store.deviceId) return toast?.('请先选择函数和设备', 'warn')
    return runArgsFlow.begin({ id: `${packageId.value}#${opts.fnName}`, name: opts.fnName, kind: 'function_library', fnName: opts.fnName, runnerId: GAMER_YAML_RUNNER_ID, entrypoint: `${packageId.value}#${opts.fnName}`, deviceId: store.deviceId, startIndex: 0 })
  }
  async function stopScript() { if (!store.runId || runStopping.value) return; runStopping.value = true; try { beginCancel(store.runId); await api.cancelRun(store.runId) } finally { runStopping.value = false } }
  async function beforePackageChange(next) {
    if (next === packageId.value) return true
    if (sourceEditorSaving.value || startPending.value) return false
    if (sourceEditorDirty.value && !await confirm('当前源码有未保存修改，放弃后切换配置包？', { title: '切换配置包', confirmText: '放弃修改', danger: true })) return false
    sourceEditorDirty.value = false; return true
  }
  function beforeTargetChange(id) { if (id === store.deviceId) return true; if (sourceEditorDirty.value || sourceEditorSaving.value || startPending.value || runArgsFlow.modal.submitting) { toast?.('请先保存或结束当前操作', 'warn'); return false } return true }
  function projectTargetConfig(id) {
    if (!beforeTargetChange(id)) return false
    const config = multiviewWorkspace?.getConfig(id) || {}, pkg = config.packageId || null
    projecting = true
    try { if (!selectPackage(pkg && packageStore.packages.some(p => p.id === pkg) ? pkg : null)) return false; runArgsFlow.close(); selScript.value = config.scriptId || ''; return true } finally { projecting = false }
  }
  watch(selScript,()=>runArgsFlow.close(),{flush:'sync'})
  watch([selScript, packageId], () => { if (!projecting && store.deviceId) multiviewWorkspace?.updateConfig(store.deviceId, { packageId: packageId.value || '', scriptId: selScript.value || '', runnerId: GAMER_YAML_RUNNER_ID,...(selScript.value!==multiviewWorkspace?.getConfig(store.deviceId)?.scriptId?{args:{}}:{}) }) }, { flush: 'post' })
  watch(packageId, (next, previous) => {
    if (restoring) return
    if (sourceEditorDirty.value || sourceEditorSaving.value) { restoring = true; restorePackage(previous); restoring = false; return }
    selScript.value = ''; runArgsFlow.close(); void refreshScripts().catch(() => {})
  }, { immediate: true, flush: 'sync' })
  watch(() => automationEditorRequest.seq, () => { if (automationEditorRequest.packageId === packageId.value && !sourceEditorDirty.value) { selScript.value = automationEditorRequest.scriptId || ''; automationTab.value = 'scripts'; void refreshScripts() } })
  watch(() => yamlResourcesChanged.seq, () => { if (yamlResourcesChanged.packageId === packageId.value) void refreshScripts().catch(() => {}) })
  async function pollRun() { if (disposed) return; if (store.runId) { try { applyRunRecord(await api.getRun(store.runId)) } catch {} } polling = setTimeout(pollRun, 1000) }
  function startRunStatusPoll() { clearTimeout(polling); if (multiviewWorkspace?.startRunStatusPoll) multiviewWorkspace.startRunStatusPoll(); else void pollRun() }
  function stopRunStatusPoll() { clearTimeout(polling); multiviewWorkspace?.stopRunStatusPoll?.() }
  function onBeforeUnload(event) { if (sourceEditorDirty.value) { event.preventDefault(); event.returnValue = '' } }
  const makePanel = kind => ({ kind, runKind: ref(kind), sourceFile: ref(''), sourceFunction: ref(''), sourcePackage: ref(''), packageId, store, selScript, automationTab, sourceEditorDirty, sourceEditorSaving, startPending, runStopping, stopScript, runScript, runFunction, runArgsFlow, configureTargetScript, startRunStatusPoll })
  onUnmounted(() => { disposed = true; fetching++; stopRunStatusPoll() })
  return { resourcePreview, closeResourcePreview: () => { resourcePreview.open = false }, runArgsFlow, onRunArgsSubmit: (...args) => runArgsFlow.confirm(...args), liveLogs, startPending, runStopping, beforePackageChange, beforeTargetChange, projectTargetConfig, refreshScripts,
    startLogPolling: () => consoleRuntime.startLogPolling?.(), stopLogPolling: () => consoleRuntime.stopLogPolling?.(), pushLog, clearCallParamsCache: () => {}, editorMatchThreshold: ref(0.9), onTemplateRenamed: () => refreshScripts(), startRunStatusPoll, stopRunStatusPoll, restoreRunState: startRunStatusPoll, onBeforeUnload, scriptPanel: makePanel('script'), functionsPanel: makePanel('func') }
}
