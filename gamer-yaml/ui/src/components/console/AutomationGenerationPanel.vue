<script setup>
import { computed, inject, onActivated, onBeforeUnmount, reactive, ref, toRef, watch } from 'vue'
import { pluginMessageChannel } from '../../../../../../web/src/workspace/plugin-messages'
import { WORKSPACE_CONTEXT_KEY } from '../../../../../../web/src/workspace/context'
import { requestAutomationContext } from './automationAiBridge'
import CandidateTemplatePreview from './CandidateTemplatePreview.vue'
import { api } from '../../../../../../web/src/api'
import { useConfirmDialog } from '../../../../../../web/src/components/ui/useConfirmDialog'
import { allSamplesPassed, sourceDiff, useGenerationState, validationLabel } from './generation-state'
import { parseSampleFile, sampleId } from './sample-import'
import { inspectSampleArchive } from '../../../../../gamer-video/ui/src/components/video/sampleBundle'
const props = defineProps({ packageId: { type: String, default: '' } })
const emit = defineEmits(['editing-state'])
const workspace = inject(WORKSPACE_CONTEXT_KEY, null)
const reviewRequest = pluginMessageChannel('gamer-yaml:open-generation')
const aiSettingsChanged = pluginMessageChannel('gamer-ai:settings-changed')
const call = (action, values) => api.callExtension('gamer-yaml', action, values)
const state = reactive(useGenerationState(toRef(props, 'packageId'), call)), confirm = useConfirmDialog()
const repairRun = ref(null)
const name = ref('automation.yaml'), goal = ref(''), samples = ref([]), selectedSamples = ref([]), importError = ref(''), importBusy = ref(false), mode = ref('generate')
const manualYaml = ref('version: 2\ntargets:\n  done:\n    template: done.png\nrun:\n  - finish: done\n    timeout: 10s\n')
const aiModel = computed(() => state.readiness?.model?.model || '')
const aiBudget = computed(() => state.readiness?.model?.default_limits)
const readinessText = computed(() => {
  if (!state.readiness) return '正在读取 AI 助手的模型与能力状态…'
  if (state.readiness.ready) return 'AI 已就绪 · 图片识别能力测试通过'
  return {
    model_not_configured: 'AI 助手尚未保存 API 密钥，请在 AI 助手中配置模型。',
    vision_probe_required: '请在 AI 助手中测试已保存的模型，确认图片识别能力。',
  }[state.readiness.reason] || state.readiness.reason || 'AI 助手暂不可用'
})
const chosen = computed(() => samples.value.filter(sample => selectedSamples.value.includes(sampleId(sample))))
const canStart = computed(() => props.packageId && chosen.value.length && goal.value.trim() && name.value.trim() && !state.busy && !state.running && !importBusy.value && (mode.value !== 'repair' || !!repairRun.value) && (mode.value === 'manual' || state.readiness?.ready === true))
const diff = computed(() => sourceDiff(state.baseline, state.yaml))
const candidateState = computed(() => ({ draft: '候选草稿', generating: '生成与修正中', validating: '全素材验证中', passed: '全部素材通过', failed: '验证未通过', cancelled: '已取消', saved: '已保存正式版本' }[state.candidate?.state] || ''))
async function importFiles(event) {
  const files = [...event.target.files], pkg = props.packageId
  importBusy.value = true; importError.value = ''
  try {
    const imported = []
    for (const file of files) {
      if (file.size > 128 * 1024 * 1024) throw new Error('单份素材包不能超过 128 MiB')
      let sample
      if (/\.json$/i.test(file.name)) {
        if (file.size > 1024 * 1024) throw new Error('JSON 内联素材限 1 MiB；较大素材请使用 .gamersample 归档')
        sample = parseSampleFile(await file.text())
      } else {
        const bundle = await inspectSampleArchive(file)
        if (pkg !== props.packageId) return
        await api.putPluginResourceBytes(pkg, 'gamer-yaml', `samples/${bundle.manifest.id}.gamersample`, bundle.bytes)
        if (pkg !== props.packageId) return
        sample = { manifest: bundle.manifest, reference: { sample_id: bundle.manifest.id, plugin_id: 'gamer-yaml' } }
      }
      if (!sampleId(sample)) throw new Error('素材缺少稳定 ID')
      imported.push(sample)
    }
    if (pkg !== props.packageId) return
    for (const sample of imported) {
      const id = sampleId(sample), previous = samples.value.find(value => sampleId(value) === id)
      if (previous && JSON.stringify(previous) !== JSON.stringify(sample)) throw new Error(`素材 ID ${id} 已存在但内容不同，请使用独立的素材 ID`)
      if (!previous) samples.value.push(sample)
      if (!selectedSamples.value.includes(id)) selectedSamples.value.push(id)
    }
  } catch (e) { if (pkg === props.packageId) importError.value = e.message }
  finally { if (pkg === props.packageId) importBusy.value = false; event.target.value = '' }
}
async function start() {
  if (!canStart.value) return
  const pkg = props.packageId, currentId = state.candidate?.id
  if (state.dirty && !await confirm('当前候选修改尚未保存，放弃并开始新的候选？', { title: '新候选', confirmText: '放弃并继续', danger: true })) return
  if (pkg !== props.packageId || currentId !== state.candidate?.id || !canStart.value) return
  await state.operation(mode.value === 'repair' ? 'generation.repair' : mode.value === 'manual' ? 'generation.create' : 'generation.start', { name: name.value.trim(), goal: goal.value.trim(), samples: chosen.value.map(sample => sample.reference || sample), ...(mode.value === 'manual' ? { yaml: manualYaml.value } : {}), ...(mode.value === 'repair' ? { run_id: repairRun.value.runId } : {}) }, { useCandidate: false })
}
async function openAiSettings() {
  pluginMessageChannel('gamer-ai:open-settings').seq++
  try { await workspace?.uiBridge.workspace.openPanel('gamer-ai:ai') } catch (e) { state.error = e.message }
}
async function selectCandidate(event) {
  const value = event.target.value, pkg = props.packageId
  if (!value || state.dirty && !await confirm('放弃当前未保存的候选源码修改？', { title: '切换候选', confirmText: '放弃并切换', danger: true })) { event.target.value = state.candidate?.id || ''; return }
  if (pkg === props.packageId) await state.select(value)
}
async function sendCandidateToAi() {
  if (!state.candidate || state.dirty || state.busy || state.running) return
  requestAutomationContext(props.packageId, { script_id: state.candidate.name, candidate_id: state.candidate.id })
  try { await workspace?.uiBridge.workspace.openPanel('gamer-ai:ai') } catch (e) { state.error = e.message }
}
async function saveFinal() {
  if (!state.canSave) return
  const pkg = props.packageId, candidate = state.candidate, id = candidate.id, revision = candidate.revision
  const approved = await confirm(`将 ${candidate.name} 与 ${candidate.templates?.length || 0} 个候选模板一起保存为正式版本？已选 ${candidate.sample_ids.length} 份素材均通过。`, { title: '保存已验证版本', confirmText: '确认保存' })
  if (approved && pkg === props.packageId && id === state.candidate?.id && revision === state.candidate.revision && state.canSave) await state.save('validated')
}
async function applyProposal() {
  const proposal = state.candidate?.pending_proposal
  if (!proposal || state.busy || state.running || state.dirty) return
  const pkg = props.packageId, id = state.candidate.id, revision = state.candidate.revision
  const approved = await confirm('将下方提议的源码与模板应用到候选？旧报告将失效，需重验全部素材并再次确认才能正式保存。', { title: '应用 AI 候选建议', confirmText: '应用到候选' })
  if (approved && pkg === props.packageId && id === state.candidate?.id && revision === state.candidate.revision && proposal.id === state.candidate.pending_proposal?.id && !state.dirty) await state.operation('generation.apply_proposal', { expected_revision: revision, proposal_id: proposal.id })
}
async function rollback(revision) {
  const pkg = props.packageId, version = state.history.version
  const approved = await confirm(`撤销变更 ${revision.id || revision.revision_id}？本次变更涉及的脚本与模板将恢复到变更前状态，保留无关资源（${revision.previous_version}）。`, { title: '撤销资源变更', confirmText: '确认撤销', danger: true })
  if (approved && pkg === props.packageId && version === state.history?.version) await state.operation('generation.rollback', { revision_id: revision.id || revision.revision_id, expected_version: version }, { useCandidate: false })
}
async function refreshSamples() {
  const pkg = props.packageId
  if (!pkg) return
  const results = await Promise.allSettled([api.callExtension('gamer-video', 'sample.list', { package_id: pkg }), api.listPluginResources(pkg, 'gamer-yaml', 'samples')])
  if (pkg !== props.packageId) return
  const records = []
  if (results[0].status === 'fulfilled') for (const item of results[0].value.samples || []) records.push({ manifest: { id: item.id, name: item.name, status: item.status, quality_warnings: item.diagnostics }, reference: { sample_id: item.id, plugin_id: 'gamer-video' } })
  if (results[1].status === 'fulfilled') for (const item of results[1].value.resources || []) {
    const match = String(item.path || item.name || '').match(/(?:^|\/)samples\/([^/]+)\.gamersample$/) || String(item.path || item.name || '').match(/^([^/]+)\.gamersample$/)
    if (match) records.push({ manifest: { id: match[1], name: match[1] }, reference: { sample_id: match[1], plugin_id: 'gamer-yaml' } })
  }
  for (const record of records) if (!samples.value.some(sample => sampleId(sample) === sampleId(record))) samples.value.push(record)
}
watch(() => props.packageId, () => { samples.value = []; selectedSamples.value = []; goal.value = ''; repairRun.value = null; mode.value = 'generate'; importError.value = ''; importBusy.value = false; void refreshSamples() }, { immediate: true, flush: 'sync' })
async function beforeTabChange() { return !state.dirty || await confirm('放弃当前未保存的候选源码修改？服务器候选仍会保留。', { title: '切换面板', confirmText: '放弃并切换', danger: true }) }
watch(() => reviewRequest.seq, async seq => {
  if (reviewRequest.packageId !== props.packageId || state.dirty || state.running) return
  if (reviewRequest.candidateId) { repairRun.value = null; mode.value = 'generate'; await state.select(reviewRequest.candidateId); return }
  const run = reviewRequest.repairRun, pkg = props.packageId
  if (!run?.runId || !run?.scriptId) return
  repairRun.value = { ...run }; mode.value = 'repair'; name.value = run.scriptId
  goal.value = '修复所选运行失败，兼容全部选中素材中的实际分支；只在控件可操作时点击，最终验证完成画面。保持原匹配阈值和全部素材的操作验证标准。'
  await Promise.allSettled([state.refresh(), refreshSamples()])
  if (seq !== reviewRequest.seq || pkg !== props.packageId) return
  const previous = state.candidates.find(c => c.name === run.scriptId && ['passed', 'saved'].includes(c.state))
  selectedSamples.value = (previous?.sample_ids || []).filter(id => samples.value.some(s => sampleId(s) === id && s.manifest.status === 'complete'))
}, { immediate: true })
watch(() => [state.dirty, state.busy], ([dirty, saving]) => emit('editing-state', { dirty, saving }), { immediate: true, flush: 'sync' })
onBeforeUnmount(() => emit('editing-state', { dirty: false, saving: false }))
onActivated(() => { void state.refresh() })
watch(() => aiSettingsChanged.seq, () => { void state.refresh() })
defineExpose({ beforeTabChange })
</script>
<template>
  <section class="generation-panel" aria-label="AI 多素材生成与离线验证">
    <header><strong>多素材生成与验证</strong><button class="btn" :disabled="state.loading" @click="state.refresh">刷新能力与候选</button></header>
    <p class="hint">无需设备。只将本次选择的素材图片、目标和候选脚本发送给已配置的模型；费用与 Token 来自 AI 助手模型配置。正式保存必须由全部选中素材验证通过。</p>
    <div class="readiness" :class="{ warning: !state.readiness?.ready }"><p role="status">{{ readinessText }}</p><p v-if="aiModel" class="hint">使用 AI 助手模型：{{ aiModel }}</p><button class="btn" @click="openAiSettings">AI 助手模型设置</button></div>
    <p v-if="state.error || importError" class="error" role="alert">{{ state.error || importError }}</p><p v-if="state.notice" class="success" role="status">{{ state.notice }}</p>
    <details :open="!!repairRun || !state.candidate"><summary>选择素材与生成目标</summary>
      <div class="form"><label>方式<select v-model="mode" class="select" :disabled="state.running || state.busy"><option value="generate">AI 生成</option><option v-if="repairRun" value="repair">AI 修复失败运行</option><option value="manual">手写源码离线验证（无需 AI）</option></select></label>
      <p v-if="mode === 'repair'" class="hint">修复运行 {{ repairRun.runId }}：使用当时保留的源码与模板版本，AI 修改候选后自动验证全部素材；通过前不修改正式脚本。</p><label>脚本文件<input v-model="name" class="input" placeholder="automation.yaml" :disabled="state.running || state.busy || mode === 'repair'" /></label>
      <label>目标<textarea v-model="goal" aria-label="生成目标" rows="3" placeholder="说明要完成什么，哪些画面证明完成" :disabled="state.running || state.busy" /></label>
      <label>导入自包含素材包（可多选）<input type="file" accept=".gamersample,.zip,.json,application/zip,application/json" multiple :disabled="state.running || state.busy || importBusy" @change="importFiles" /></label>
      <p class="hint">可直接选择视频工作台素材，或导入 .gamersample 归档。原视频本身不包含操作与终态证据，不能冒充完整验证素材。</p>
      <button class="btn" :disabled="state.busy || state.running" @click="refreshSamples">刷新可用素材</button>
      <ul class="samples"><li v-for="sample in samples" :key="sampleId(sample)"><label><input v-model="selectedSamples" type="checkbox" :value="sampleId(sample)" :disabled="state.running || state.busy || (sample.manifest.status && sample.manifest.status !== 'complete')" />{{ sample.manifest.name || sampleId(sample) }} · {{ sample.reference ? sample.reference.plugin_id : `${sample.files.length} 个资源` }}</label><p v-for="(warning,index) in sample.manifest.warnings || sample.manifest.quality_warnings || []" :key="index" class="warning">{{ typeof warning === 'string' ? warning : JSON.stringify(warning) }}</p></li></ul>
      <textarea v-if="mode === 'manual'" v-model="manualYaml" class="source" aria-label="手写候选 YAML" rows="10" spellcheck="false" />
      <p v-if="mode !== 'manual' && aiBudget" class="hint">沿用 AI 助手对话默认预算：{{ aiBudget.max_turns }} 轮、{{ aiBudget.max_seconds }} 秒、{{ aiBudget.max_tokens === 0 ? '累计 Token 不限制' : `${aiBudget.max_tokens} Token` }}、连续失败 {{ aiBudget.max_failures }} 次。生成用量单独统计。</p>
      <button class="btn btn-primary" :disabled="!canStart" @click="start">{{ mode === 'repair' ? '开始 AI 修复并验证全部素材' : mode === 'manual' ? '建立候选用于验证' : '开始生成并验证全部素材' }}</button>
      </div>
    </details>
    <label class="candidate-picker">候选记录<select :value="state.candidate?.id || ''" class="select" :disabled="state.busy || state.running" @change="selectCandidate"><option value="">选择候选…</option><option v-for="item in state.candidates" :key="item.id" :value="item.id">{{ item.name }} · {{ item.state }} · {{ item.id }}</option></select></label>
    <section v-if="state.candidate" class="candidate" aria-label="候选详情">
      <header><strong>{{ candidateState }}</strong><span>尝试 {{ state.candidate.attempts }} / {{ state.candidate.limits?.max_attempts ?? '未知' }} · {{ state.candidate.known_tokens }} Token{{ state.candidate.limits?.max_tokens === 0 ? ' · 累计 Token 不限制' : ` / ${state.candidate.limits?.max_tokens ?? '未知'}` }} · 最多 {{ state.candidate.limits?.max_seconds ?? '未知' }}s</span></header>
      <p v-if="state.running && state.candidate.phase" role="status" class="hint">{{ { retrying_model: '模型请求中断，正在重试修复；可随时取消', validating_failure_source: '正在回放失败时的原版本', checking_samples: '正在核对素材来源', preparing_images: '正在本地压缩图片，保留原始分辨率', requesting_model: '正在等待模型生成', validating_samples: '正在回放验证全部素材' }[state.candidate.phase] || '' }}</p>
      <p v-if="state.candidate.model_input" class="hint">图片保留原始分辨率 · {{ state.candidate.model_input.full_frames }} 张整图 + {{ state.candidate.model_input.detail_frames }} 张无损局部图 · 图片 {{ (state.candidate.model_input.image_bytes / 1024 / 1024).toFixed(2) }} MiB · 请求 {{ (state.candidate.model_input.request_bytes / 1024 / 1024).toFixed(2) }} MiB</p>
      <p v-if="state.candidate.explanation" class="hint">{{ state.candidate.explanation }}</p><p v-if="state.candidate.unknown_usage" class="warning">部分模型 Token 用量未知，累计统计不完整</p>
      <p v-if="state.candidate.reason" class="warning">{{ state.candidate.reason }}</p><p class="hint">{{ state.candidate.name }} · r{{ state.candidate.revision }} · 固定素材：{{ state.candidate.sample_ids.join(', ') }}</p>
      <section v-if="state.candidate.pending_proposal" class="proposal" aria-label="待审核 AI 修改建议"><strong>待审核建议（尚未修改当前源码）</strong><p>{{ state.candidate.pending_proposal.proposal.explanation }}</p><pre>{{ sourceDiff(state.candidate.yaml, state.candidate.pending_proposal.proposal.yaml) }}</pre><details><summary>提议的完整 YAML 与模板</summary><pre>{{ state.candidate.pending_proposal.proposal.yaml }}</pre><ul><li v-for="template in state.candidate.pending_proposal.proposal.templates || []" :key="template.name">{{ template.name }} · {{ template.sample_id }} / {{ template.frame_id }} · {{ template.rect?.join(', ') }}<CandidateTemplatePreview :package-id="packageId" :candidate-id="state.candidate.id" :revision="state.candidate.revision" :proposal-id="state.candidate.pending_proposal.id" :name="template.name" /></li></ul></details><button class="btn" :disabled="state.busy || state.running || state.dirty || state.candidate.pending_proposal.base_revision !== state.candidate.revision" @click="applyProposal">审核并应用到候选</button><p v-if="state.candidate.pending_proposal.base_revision !== state.candidate.revision" class="warning">建议所依据的候选版本已变化，请重新分析</p></section>
      <div class="actions"><button class="btn" :disabled="state.busy || state.running || state.dirty || !state.readiness?.ready" @click="sendCandidateToAi">交给 AI 审阅候选</button><button v-if="state.running" class="btn btn-danger" :disabled="state.busy" @click="state.operation('generation.cancel')">取消</button><button v-else class="btn" :disabled="state.busy || !state.readiness?.ready || state.dirty || state.candidate.state === 'saved'" @click="state.operation('generation.retry')">继续 AI 修正</button><button class="btn" :disabled="state.busy || state.running || state.candidate.state === 'saved'" @click="state.validate">验证全部素材</button><button class="btn" :disabled="state.busy || state.running || state.candidate.state === 'saved'" @click="state.save('draft')">保留候选草稿</button><button class="btn btn-primary" :disabled="!state.canSave" @click="saveFinal">确认保存正式版本</button></div>
      <p v-if="state.dirty" class="warning">源码已修改，旧验证报告已失效；保存修改后重新验证全部素材</p>
      <textarea v-model="state.yaml" class="source" aria-label="候选 YAML" spellcheck="false" rows="14" :disabled="state.busy || state.running || state.candidate.state === 'saved'" />
      <details v-if="state.candidate.repair"><summary>失败原版本的验证结果</summary><p>来源运行：{{ state.candidate.repair.run_id }}</p><pre>{{ JSON.stringify(state.candidate.repair.baseline_report, null, 2) }}</pre></details><details><summary>源码差异（相对冻结的正式资源）</summary><p v-if="!state.baselineReady" class="warning">正在读取原始版本，读取完成前不能正式保存</p><template v-else><p v-if="!state.baseExists" class="hint">这是新脚本，正式资源中没有旧版本</p><pre>{{ diff }}</pre></template></details>
      <details><summary>候选模板与证据来源（{{ state.candidate.templates?.length || 0 }}）</summary><ul><li v-for="template in state.candidate.templates || []" :key="template.name">{{ template.name }} · {{ template.sample_id }} / {{ template.frame_id }} · {{ template.rect?.join(', ') }}<CandidateTemplatePreview :package-id="packageId" :candidate-id="state.candidate.id" :revision="state.candidate.revision" :name="template.name" /></li></ul></details>
      <section v-if="state.verificationScope" class="verification-scope" aria-label="验证范围与冻结设置">
        <strong>本次验证范围</strong>
        <p v-if="state.dirty || !state.candidate.report || state.verificationScope.validation_status !== 'passed'" class="warning">当前没有有效的全部通过报告；下列内容仅说明本次受测范围</p>
        <p>受测参数（JSON）</p><pre aria-label="受测参数">{{ JSON.stringify(state.verificationScope.tested_args ?? {}, null, 2) }}</pre>
        <p v-if="state.verificationScope.default_args_match === false" class="warning" role="status">默认参数未验证：本次受测参数与脚本默认值不同，不能把本报告用于默认参数运行</p>
        <p v-else-if="state.verificationScope.default_args_match === true" class="hint">本次受测参数与脚本默认参数一致</p>
        <p v-else class="warning">尚未确认受测参数是否与脚本默认值一致</p>
        <p aria-label="受测执行设置">受测执行设置（冻结）：默认超时 {{ state.verificationScope.execution_settings?.default_timeout_secs ?? '未知' }} 秒 · 点击前延迟 {{ state.verificationScope.execution_settings?.before_click_ms ?? '未知' }} ms · 点击后延迟 {{ state.verificationScope.execution_settings?.after_click_ms ?? '未知' }} ms</p>
        <p class="hint">其他参数、未覆盖分支和真实设备／模型行为均未由本次离线验证证明。源码、模板、参数或执行设置变化后需重新验证。</p>
        <details><summary>受测素材 ID 与内容指纹（{{ state.verificationScope.samples?.length || 0 }}）</summary><ul><li v-for="sample in state.verificationScope.samples || []" :key="sample.id"><strong>{{ sample.id }}</strong><pre>{{ sample.content_sha256 }}</pre></li></ul></details>
      </section>
      <section v-if="state.candidate.report" class="report" aria-label="全素材验证报告"><h4>{{ state.dirty ? '过期报告' : allSamplesPassed(state.candidate) ? '全部素材通过' : '尚未达到正式保存条件' }}</h4><article v-for="sample in state.candidate.report.samples || []" :key="sample.sample_id"><strong>{{ sample.sample_id }} · {{ validationLabel(sample.status) }}</strong><p v-for="(diagnostic,index) in sample.diagnostics || []" :key="index">{{ diagnostic.path || diagnostic.step_path || '' }} {{ diagnostic.message || diagnostic }}</p></article><p v-for="(diagnostic,index) in state.candidate.report.diagnostics || []" :key="index" class="warning">{{ diagnostic.message || diagnostic }}</p><p class="hint">通过只说明所选素材记录的路径与目标成立，不能证明未出现分支或任意新操作的效果。</p></section>
    </section>
    <details><summary>脚本与模板版本历史</summary><ul><li v-for="revision in state.history?.revisions || []" :key="revision.id || revision.revision_id"><span>{{ revision.name || revision.id || revision.revision_id }} · {{ revision.at || revision.created_at }}</span><button class="btn" :disabled="state.busy || state.running" @click="rollback(revision)">撤销此变更</button></li></ul><p v-if="!state.history?.revisions?.length" class="hint">暂无已保存版本</p></details>
  </section>
</template>
<style scoped>
.verification-scope{display:flex;flex-direction:column;gap:7px;padding:10px;border:1px solid var(--border);border-radius:4px;line-height:1.7}.verification-scope pre{margin:0}.proposal{border:1px solid var(--accent);border-radius:5px;padding:10px;display:flex;flex-direction:column;gap:8px}.generation-panel{display:flex;flex:1;min-height:0;overflow:auto;flex-direction:column;gap:12px;padding:4px;font-size:12px}header,.actions,.candidate-picker{display:flex;gap:8px;align-items:center;flex-wrap:wrap}header strong{flex:1}.form,.candidate,.report{display:flex;flex-direction:column;gap:10px}.form{padding:10px 0}.form>label{display:flex;flex-direction:column;gap:5px}.candidate-picker select{flex:1;min-width:100px}.hint{color:var(--text-2);line-height:1.7}.warning{color:var(--warn)}.error{color:var(--danger)}.success{color:var(--ok)}.readiness{padding:9px;background:var(--bg-0);border:1px solid var(--border);border-radius:4px}.samples{padding-left:18px}.limits{display:flex;gap:8px;flex-wrap:wrap;border:1px solid var(--border);padding:8px}.limits label{display:flex;align-items:center;gap:5px}.limits input{width:75px}.source,textarea,pre{font-family:var(--mono,monospace);font-size:12px;line-height:1.6;box-sizing:border-box;background:var(--bg-0);color:var(--text-0);border:1px solid var(--border);border-radius:4px;padding:8px;white-space:pre-wrap;overflow-wrap:anywhere}.source{width:100%;min-height:160px;resize:vertical}pre{max-height:280px;overflow:auto}.report article{border-left:2px solid var(--border);padding-left:10px}.report h4{margin:0}summary{cursor:pointer;line-height:1.8}li{margin:5px 0}input,select,textarea{accent-color:var(--accent)}:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
</style>
