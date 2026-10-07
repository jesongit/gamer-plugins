import { computed, onScopeDispose, ref, watch } from 'vue'

export const candidateRunning = candidate => ['generating', 'validating'].includes(candidate?.state)
export const validationLabel = status => ({ passed: '通过', failed: '逻辑或匹配失败', insufficient_evidence: '证据不足', unsupported: '不支持' }[status] || '未验证')
export function allSamplesPassed(candidate) {
  const ids = candidate?.sample_ids || [], report = candidate?.report
  return ['passed', 'saved'].includes(candidate?.state) && report?.status === 'passed' && ids.length > 0
    && new Set(ids).size === ids.length && report.samples?.length === ids.length
    && ids.every(id => report.samples.filter(sample => sample.sample_id === id && sample.status === 'passed').length === 1)
}
export function sourceDiff(before = '', after = '') {
  const left = before.split('\n'), right = after.split('\n')
  let start = 0; while (start < left.length && start < right.length && left[start] === right[start]) start++
  let endLeft = left.length, endRight = right.length
  while (endLeft > start && endRight > start && left[endLeft - 1] === right[endRight - 1]) { endLeft--; endRight-- }
  if (start === left.length && start === right.length) return '没有源码变化'
  return [`@@ 第 ${start + 1} 行 @@`, ...left.slice(start, endLeft).map(line => `- ${line}`), ...right.slice(start, endRight).map(line => `+ ${line}`)].join('\n')
}
export function useGenerationState(packageId, call) {
  const readiness = ref(null), candidates = ref([]), candidate = ref(null), history = ref(null), verificationScope = ref(null)
  const error = ref(''), busy = ref(false), loading = ref(false), yaml = ref(''), baseline = ref(''), baselineReady = ref(false), baseExists = ref(false), notice = ref('')
  let scope = 0, request = 0, pollRequest = 0, refreshRequest = 0, timer, disposed = false
  const dirty = computed(() => !!candidate.value && yaml.value !== candidate.value.yaml)
  const running = computed(() => candidateRunning(candidate.value))
  const canSave = computed(() => !busy.value && !dirty.value && baselineReady.value && candidate.value?.state === 'passed' && allSamplesPassed(candidate.value))
  function acceptBase(result) {
    verificationScope.value = result.verification_scope && typeof result.verification_scope === 'object' && !Array.isArray(result.verification_scope) ? result.verification_scope : null
    if (typeof result.base_yaml === 'string' && typeof result.base_exists === 'boolean') { baseline.value = result.base_yaml; baseExists.value = result.base_exists; baselineReady.value = true }
  }
  function accept(value) {
    if (!value || value.package_id !== packageId.value) return false
    if (candidate.value?.id === value.id && Number(value.revision) < Number(candidate.value.revision)) return false
    candidate.value = value; yaml.value = value.yaml || ''
    candidates.value = [value, ...candidates.value.filter(item => item.id !== value.id)]
    return true
  }
  async function refresh() {
    const token = scope, serial = ++refreshRequest, pkg = packageId.value
    if (!pkg || disposed) return
    loading.value = true
    const results = await Promise.allSettled([
      call('generation.readiness', {}), call('generation.list', { package_id: pkg }), call('generation.history', { package_id: pkg }),
    ])
    if (token !== scope || serial !== refreshRequest) return
    readiness.value = results[0].status === 'fulfilled' ? results[0].value : { ready: false, reason: results[0].reason?.message || 'AI 插件状态无法读取' }
    if (results[1].status === 'fulfilled') candidates.value = results[1].value.candidates || []
    else error.value = results[1].reason?.message || '无法读取候选'
    if (results[2].status === 'fulfilled') history.value = results[2].value
    loading.value = false
  }
  async function select(id) {
    if (disposed) return false
    const token = scope, serial = ++request; pollRequest++; error.value = ''; notice.value = ''; busy.value = true
    candidate.value = null; verificationScope.value = null; yaml.value = ''; baseline.value = ''; baselineReady.value = false; baseExists.value = false
    try {
      const result = await call('generation.get', { package_id: packageId.value, candidate_id: id })
      if (token !== scope || serial !== request) return false
      if (accept(result.candidate)) { acceptBase(result); return true }
    } catch (e) { if (token === scope && serial === request) error.value = e.message }
    finally { if (token === scope && serial === request) busy.value = false }
    return false
  }
  async function operation(action, extra = {}, { useCandidate = true } = {}) {
    if (disposed || !packageId.value || busy.value) return false
    const token = scope, serial = ++request, current = candidate.value; pollRequest++
    if (useCandidate && !current) return false
    busy.value = true; error.value = ''; notice.value = ''
    const values = { package_id: packageId.value, ...(useCandidate ? { candidate_id: current.id } : {}), ...extra }
    try {
      const result = await call(action, values)
      if (token !== scope || serial !== request) return false
      if (['generation.start', 'generation.create', 'generation.repair'].includes(action)) { baseline.value = ''; baselineReady.value = false; baseExists.value = false }
      if (result.candidate && accept(result.candidate)) acceptBase(result)
      if (action === 'generation.rollback') { notice.value = '版本已回退；请重新加载编辑器与模板'; await refresh() }
      if (action === 'generation.save') { notice.value = extra.mode === 'validated' ? '已原子保存已验证脚本、模板与版本记录' : '候选草稿已保留，未写入正式资源'; await refresh() }
      return result
    } catch (e) { if (token === scope && serial === request) error.value = e.message || '请求失败'; return false }
    finally { if (token === scope && serial === request) busy.value = false }
  }
  async function saveEdits() {
    if (!dirty.value) return true
    return !!await operation('generation.edit', { expected_revision: candidate.value.revision, yaml: yaml.value })
  }
  async function validate() { if (await saveEdits()) return operation('generation.validate') }
  async function save(mode) {
    if (mode === 'validated' && !canSave.value) return false
    if (!await saveEdits()) return false
    return operation('generation.save', { mode, expected_revision: candidate.value.revision, expected_version: candidate.value.base_version })
  }
  async function poll() {
    clearTimeout(timer)
    if (disposed) return
    if (candidate.value && !busy.value && !dirty.value) {
      const token = scope, serial = ++pollRequest, current = candidate.value
      try {
        const result = await call('generation.get', { package_id: packageId.value, candidate_id: current.id })
        if (token === scope && serial === pollRequest && current.id === candidate.value?.id && !busy.value && !dirty.value) { if (accept(result.candidate)) acceptBase(result) }
      } catch (e) { if (token === scope && serial === pollRequest) error.value = e.message }
    }
    if (!disposed) timer = setTimeout(poll, running.value ? 700 : 3000)
  }
  watch(packageId, () => {
    scope++; request++; pollRequest++; candidate.value = null; verificationScope.value = null; candidates.value = []; history.value = null; readiness.value = null
    yaml.value = ''; baseline.value = ''; baselineReady.value = false; baseExists.value = false; error.value = ''; notice.value = ''; busy.value = false; loading.value = false
    void refresh()
  }, { immediate: true, flush: 'sync' })
  timer = setTimeout(poll, 700)
  onScopeDispose(() => { disposed = true; scope++; request++; pollRequest++; clearTimeout(timer) })
  return { readiness, candidates, candidate, history, verificationScope, error, busy, loading, yaml, baseline, baselineReady, baseExists, notice, dirty, running, canSave, refresh, select, operation, saveEdits, validate, save }
}
