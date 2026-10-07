// 运行参数流程（阶段 5，plan §12.1/§12.2；P12.3 起参数声明走服务端 schema API）：
// Console 手动运行与独立页函数测试共用的
// 「取 schema → 仅补齐必填值 → 稀疏 args 提交 → 400 诊断回填字段 → 202 摘要+建议缓存」状态机。
//
// 宿主注入：
// - exec({ id, name, kind, fnName, startIndex, args }) → 202 回复（宿主完成 API 调用与
//   run_id 登记/轮询启动；抛错时 400 invalid_args 由本流程消化，其余原样抛回宿主处理）；
// - saveConfiguration(opts): begin({mode:'configure'}) 的仅保存入口；永不调用 exec，包含无参数脚本。
// - useConfiguredArgs: 显式目标配置的合法覆盖值完整保留，且不读写跨目标建议缓存。
// - notify({ rep, args, summary })：202 成功后的页面反馈（toast/日志区摘要）；
// - loadParams({ runnerId, entrypoint })：参数 schema descriptor 获取（缺省走
//   api.getEntrypointParams，契约 §7——前端不为取参数而解析 YAML）。
// 纯流程不持有路由/toast 单例，vitest 可注入 loadParams 直测。
import { reactive } from 'vue'
import { api } from '../../../../../web/src/api'
import {
  describeResolvedArgs, loadRunArgsSuggestion,
  mapArgDiagnostics, saveRunArgsSuggestion, validateArgsAgainstParams,
} from '../script-editor/params'
import { schemaToParamDecls } from '../script-editor/entrypointParams'
import { checkLiteral, hasParamDefault } from '../script-editor/schema'

/** schema 加载失败 → 结构化 Error（code/diagnostics 供宿主分型展示）。 */
function toParamsLoadError(e, { runnerId, entrypoint }) {
  const data = e && typeof e.data === 'object' ? e.data : null
  const code = data?.error || e?.code || ''
  if (code === 'invalid_script') {
    const diagnostics = Array.isArray(data.diagnostics) ? data.diagnostics : []
    const first = diagnostics.find((d) => d && typeof d.message === 'string' && d.message)
    const err = new Error(`参数声明无法解析：${first ? first.message : '脚本校验未通过'}（${entrypoint}）`)
    err.code = 'invalid_script'
    err.diagnostics = diagnostics
    return err
  }
  if (code === 'not_found') {
    const err = new Error(`运行目标不存在或已删除：${data?.resource || entrypoint}`)
    err.code = 'not_found'
    return err
  }
  if (code === 'runner_not_found') {
    const err = new Error(`执行器未注册：${data?.runner_id || runnerId}（扩展未安装或未启动）`)
    err.code = 'runner_not_found'
    return err
  }
  return e // 网络/登录等其余错误原样上抛
}

export function useRunArgsFlow({ exec, saveConfiguration, notify = () => {}, storage = undefined, loadParams = undefined } = {}) {
  const loadDescriptor = loadParams
    || (({ runnerId, entrypoint }) => api.getEntrypointParams(runnerId, entrypoint))
  let generation = 0
  let declaredParams = []
  let suppliedRequiredArgs = {}
  let saveSuggestions = true
  // 弹窗态（RunParamsModal props 直接绑定 modal.*）
  const modal = reactive({
    open: false,
    mode: 'run',
    title: '运行参数',
    desc: '',
    submitLabel: '▶ 运行',
    targetId: '',       // 脚本 id 或函数库文件 id（<pkg>/<file>.yaml）
    targetName: '',
    kind: 'script',     // 'script' | 'function_library'
    fnName: null,       // 函数测试目标函数名
    deviceId: null,
    startIndex: 0,
    params: [],
    initialArgs: {},
    suggestions: {},
    templates: [],
    fieldErrors: {},    // 服务端 400 诊断按参数名映射
    generalErrors: [],
    submitting: false,
    loading: false,     // schema descriptor 加载中（begin 防并发重入）
  })

  function reset() {
    modal.open = false
    modal.desc = ''
    modal.fieldErrors = {}
    modal.generalErrors = []
    modal.initialArgs = {}
  }

  function close() {
    if (modal.submitting) return
    generation++
    modal.loading = false
    reset()
  }

  /**
   * 发起一次运行/测试：先经服务端 entrypoint schema API 取参数声明（404/400 等
   * 加载失败 → 结构化 Error 上抛，宿主提示，不弹参数框）；缺少必填值 → 打开表单
   * （返回 {form:true} 等待用户提交）；其余直接 exec，默认值交由服务端采用。
   * submitting 期间与表单打开时忽略重复发起。
   */
  async function begin(opts = {}) {
    if (modal.open || modal.submitting || modal.loading) return { form: false, busy: true }
    const request = ++generation
    saveSuggestions = !opts.useConfiguredArgs && opts.mode !== 'configure'
    const kind = opts.kind || 'script'
    const runnerId = opts.runnerId || ''
    const entrypoint = opts.entrypoint || ''
    Object.assign(modal, {
      mode: opts.mode === 'configure' ? 'configure' : 'run',
      title: opts.title || (kind === 'function_library' ? '测试函数参数' : '运行参数'),
      desc: opts.desc || '',
      submitLabel: opts.submitLabel || (kind === 'function_library' ? '▶ 测试' : '▶ 运行'),
      targetId: opts.id || '',
      targetName: opts.name || opts.id || '',
      kind,
      fnName: opts.fnName ?? null,
      deviceId: opts.deviceId ?? null,
      startIndex: opts.startIndex || 0,
      initialArgs: {},
    })
    modal.loading = true
    let descriptor
    try {
      descriptor = await loadDescriptor({ runnerId, entrypoint })
    } catch (e) {
      if (request !== generation) return { form: false, cancelled: true }
      // 加载失败：清空弹窗态（含上一次目标的参数残留）→ 结构化错误上抛交宿主提示
      modal.params = []
      modal.suggestions = {}
      modal.templates = []
      reset()
      throw toParamsLoadError(e, { runnerId, entrypoint })
    } finally {
      if (request === generation) modal.loading = false
    }
    if (request !== generation) return { form: false, cancelled: true }
    declaredParams = schemaToParamDecls(descriptor?.schema)
    const required = declaredParams.filter(param => param.required && !hasParamDefault(param))
    suppliedRequiredArgs = Object.fromEntries((opts.useConfiguredArgs ? declaredParams : required).filter(param => {
      const value = opts.initialArgs?.[param.name]
      return (opts.useConfiguredArgs ? Object.hasOwn(opts.initialArgs || {}, param.name) : value !== undefined && value !== null && value !== '') && !checkLiteral(param.type, value)
    }).map(param => [param.name, opts.initialArgs[param.name]]))
    // 手动运行直接采用声明默认值，只补齐未提供的必填值。
    modal.params = required.filter(param => !Object.hasOwn(suppliedRequiredArgs, param.name))
    const suggestions = opts.useConfiguredArgs ? {} : loadRunArgsSuggestion(opts.id || '', storage)
    modal.suggestions = Object.fromEntries(modal.params.filter(param => Object.hasOwn(suggestions, param.name))
      .map(param => [param.name, suggestions[param.name]]))
    modal.templates = opts.templates || []
    if (modal.mode === 'configure') {
      suppliedRequiredArgs = {}
      modal.params = declaredParams
      modal.initialArgs = JSON.parse(JSON.stringify(opts.initialArgs || {}))
      modal.suggestions = {}
      modal.open = true
      return { form: true, configuration: true }
    }
    if (!modal.params.length) {
      await run(undefined)
      return { form: false }
    }
    modal.open = true
    return { form: true }
  }

  /** RunParamsModal 提交（已过客户端校验）：稀疏 args → 服务端。 */
  async function confirm(args) {
    if (!modal.open || modal.submitting || modal.loading) return { ok: false, reason: 'inactive' }
    return run(args)
  }

  async function run(args) {
    modal.submitting = true
    modal.fieldErrors = {}
    modal.generalErrors = []
    const explicit = { ...suppliedRequiredArgs }
    for (const param of modal.params) {
      if (args && Object.hasOwn(args, param.name)) explicit[param.name] = args[param.name]
    }
    args = Object.keys(explicit).length ? explicit : undefined
    const opts = {
      id: modal.targetId,
      name: modal.targetName,
      kind: modal.kind,
      fnName: modal.fnName,
      startIndex: modal.startIndex,
      args,
      ...(modal.deviceId ? { deviceId: modal.deviceId } : {}),
    }
    try {
      if (modal.mode === 'configure') {
        const errors = validateArgsAgainstParams(declaredParams, args)
        if (errors.length) {
          modal.fieldErrors = Object.fromEntries(errors.map(error => [error.name, [error.message]]))
          return { ok: false, reason: 'invalid_args' }
        }
        if (!saveConfiguration) throw new Error('当前页面不支持保存目标运行配置')
        const result = await saveConfiguration(opts)
        modal.open = false
        return { ok: true, configuration: true, result }
      }
      const rep = await exec(opts)
      modal.open = false
      // 仅补填的必填值进建议缓存；声明默认值不写入，也不被旧覆盖缓存遮蔽。
      if (saveSuggestions && args && Object.keys(args).length) saveRunArgsSuggestion(modal.targetId, args, storage)
      notify({
        deviceId: opts.deviceId,
        rep,
        args,
        summary: describeResolvedArgs(declaredParams, args, rep?.resolved_args),
      })
      return { ok: true, rep }
    } catch (e) {
      if (modal.mode === 'configure') {
        modal.generalErrors = [e.message || String(e)]
        return { ok: false, reason: 'configuration_error' }
      }
      if (e && e.status === 400 && e.data && e.data.error === 'invalid_args' && modal.open) {
        const mapped = mapArgDiagnostics(
          e.data.diagnostics,
          modal.params.map((p) => p.name),
        )
        modal.fieldErrors = mapped.byName
        const general = [...mapped.other]
        if (e.data.message) general.unshift(String(e.data.message))
        modal.generalErrors = general
        return { ok: false, reason: 'invalid_args', diagnostics: e.data.diagnostics || [] }
      }
      // 设备占用 409 等其他错误：关闭表单，交宿主统一处理（冲突弹窗/报错提示）；
      // invalid_args 但表单未打开（无参数直跑路径）同样上抛——否则诊断无处展示、点运行看似无反应
      reset()
      throw e
    } finally {
      modal.submitting = false
    }
  }

  return { modal, begin, confirm, close }
}
