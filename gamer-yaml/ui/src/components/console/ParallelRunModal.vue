<script setup>
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { api } from '../../../../../../web/src/api'
import { devicesData, applyRunRecord, findRun, runRegistry } from '../../../../../../web/src/store'
import { vBackdropDismiss } from '../../../../../ui-shared/backdrop-dismiss.js'
import ParamsForm from '../../script-editor/components/ParamsForm.vue'
import { schemaToParamDecls } from '../../script-editor/entrypointParams'
import { submitParallelRuns } from '../../composables/parallel-runs'
import { GAMER_YAML_RUNNER_ID } from '../../gamer-yaml-runner'

const props = defineProps({ open: Boolean, packageId: { type: String, required: true }, entrypoint: { type: String, required: true }, currentDevice: String })
const emit = defineEmits(['close'])
const selected = ref([]), params = ref([]), form = ref(null), loading = ref(false), submitting = ref(false), error = ref(''), results = ref([])
const targets = computed(() => devicesData.value)
const submitted = computed(() => results.value.length > 0)
const accepted = computed(() => results.value.filter(r => r.runId).length)
let request = 0, timer, disposed = false
const pending = new Set()
const getActiveRun = id => findRun(runRegistry.activeByDevice[id])
const unavailable = id => !!getActiveRun(id) || pending.has(id)
const rowState = result => {
  const run = result.runId && findRun(result.runId)
  const state = run?.state || result.status
  return ({ starting: '准备中', started: '已启动', running: '运行中', paused: '已暂停', stopping: '正在停止', success: '成功', failed: '失败', cancelled: '已停止', skipped: '跳过', error: '启动失败' })[state] || state
}
const targetName = id => targets.value.find(t => t.id === id)?.name || id
function close() { if (!submitting.value) emit('close') }
async function poll() {
  clearTimeout(timer)
  const runIds = results.value.filter(r => r.runId && !['success', 'failed', 'cancelled'].includes(findRun(r.runId)?.state)).map(r => r.runId)
  await Promise.allSettled(runIds.map(async id => applyRunRecord(await api.getRun(id))))
  if (!disposed && props.open && runIds.length) timer = setTimeout(poll, 1500)
}
watch(() => [props.open, props.packageId, props.entrypoint], async ([open]) => {
  const token = ++request
  clearTimeout(timer)
  if (!open) return
  error.value = ''; results.value = []; params.value = []; loading.value = true
  selected.value = targets.value.some(t => t.id === props.currentDevice && !unavailable(t.id)) ? [props.currentDevice] : []
  try {
    const descriptor = await api.getEntrypointParams(GAMER_YAML_RUNNER_ID, props.entrypoint)
    if (token !== request) return
    if (!Array.isArray(descriptor?.schema)) throw new Error('未读取到脚本参数，请刷新后重试')
    params.value = schemaToParamDecls(descriptor.schema)
  } catch (e) { if (token === request) error.value = e.message }
  finally { if (token === request) loading.value = false }
}, { immediate: true })
async function start() {
  if (loading.value || submitting.value || submitted.value || error.value || !selected.value.length) return
  if (!form.value || form.value.validate().length) return
  const config = { packageId: props.packageId, entrypoint: props.entrypoint, runnerId: GAMER_YAML_RUNNER_ID, args: form.value.getArgs() }
  const ids = selected.value.filter(id => targets.value.some(t => t.id === id))
  submitting.value = true
  try {
    await submitParallelRuns({ targetIds: ids, config, pending, active: getActiveRun, submit: api.run, onResult: result => {
      const index = results.value.findIndex(r => r.targetId === result.targetId)
      if (index === -1) results.value.push(result); else results.value[index] = result
      if (result.runId) applyRunRecord({ ...result.record, run_id: result.runId, state: result.record?.state || 'starting', device_id: result.targetId, runner_id: config.runnerId, entrypoint: config.entrypoint })
    } })
    void poll()
  } finally { submitting.value = false }
}
onBeforeUnmount(() => { disposed = true; request++; clearTimeout(timer) })
</script>
<template>
  <div v-if="open" class="modal-mask" v-backdrop-dismiss="close">
    <section class="modal parallel-modal" role="dialog" aria-modal="true" aria-labelledby="parallel-title">
      <div class="modal-head"><h2 id="parallel-title" class="title">多账号运行</h2><button class="btn btn-ghost btn-sm" :disabled="submitting" aria-label="关闭多账号运行" @click="close">关闭</button></div>
      <div class="modal-body">
        <p class="script-name">{{ entrypoint }}</p>
        <p class="hint">所选账号同时运行，共用本次参数。每个账号分别记录结果。</p>
        <p v-if="error" role="alert" class="error">{{ error }}</p>
        <template v-if="!submitted">
          <div class="account-heading"><h3>选择账号</h3><button class="btn btn-sm btn-ghost" @click="selected = targets.filter(t => !unavailable(t.id)).map(t => t.id)">选择全部空闲账号</button><button class="btn btn-sm btn-ghost" @click="selected = []">清空</button></div>
          <div v-if="targets.length" class="account-list">
            <label v-for="target in targets" :key="target.id" class="account-row">
              <input v-model="selected" type="checkbox" :value="target.id" :disabled="unavailable(target.id)" />
              <span>{{ target.name || target.id }}</span><span class="account-state">{{ unavailable(target.id) ? '已有运行' : target.status === 'online' ? '在线' : '运行时连接' }}</span>
            </label>
          </div>
          <p v-else class="hint">暂无账号，请先添加游戏目标。</p>
          <h3>共用参数</h3>
          <p v-if="loading" role="status">正在读取参数…</p>
          <ParamsForm v-else-if="!error" ref="form" :params="params" />
        </template>
        <div v-else aria-live="polite" class="result-list">
          <div v-for="result in results" :key="result.targetId" class="result-row">
            <span>{{ targetName(result.targetId) }}</span><span :class="{ error: ['error', 'failed'].includes(findRun(result.runId)?.state || result.status) }">{{ rowState(result) }}</span>
            <span v-if="result.reason || findRun(result.runId)?.error" class="result-error">{{ result.reason || findRun(result.runId)?.error }}</span>
          </div>
          <p class="hint">已提交 {{ accepted }} 个账号。关闭窗口后，已启动的脚本继续运行；可在任务与运行日志中查看。</p>
        </div>
      </div>
      <div class="modal-foot"><span class="selection-count" role="status">{{ submitted ? `已提交 ${accepted} 个账号` : `已选 ${selected.length} 个账号` }}</span><button class="btn" :disabled="submitting" @click="close">{{ submitted ? '关闭' : '取消' }}</button><button v-if="!submitted" class="btn btn-primary" :disabled="loading || submitting || !!error || !selected.length" @click="start">{{ submitting ? '提交中…' : `同时运行（${selected.length}）` }}</button></div>
    </section>
  </div>
</template>
<style scoped>
.parallel-modal{width:620px;max-width:calc(100vw - 24px)}
.script-name{margin:0 0 6px;font-weight:600;overflow-wrap:anywhere}
h3{font-size:14px;margin:18px 0 8px}.account-heading{display:flex;align-items:center;gap:6px;flex-wrap:wrap;margin-top:18px}.account-heading h3{margin:0 auto 0 0}
.account-list{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:0 16px;margin-top:6px}.account-row,.result-row{display:flex;align-items:center;gap:10px;padding:10px 0;border-bottom:1px solid var(--border);min-width:0}.account-row span{overflow-wrap:anywhere}.account-row input{accent-color:var(--accent)}.account-state{color:var(--text-2);font-size:12px;margin-left:auto}.result-row{justify-content:space-between;flex-wrap:wrap}.result-error{width:100%;font-size:13px;overflow-wrap:anywhere;color:var(--danger)}.selection-count{margin-right:auto;color:var(--text-1);font-size:13px}.hint{font-size:13px;color:var(--text-2);line-height:1.6}.error{color:var(--danger)}
@media(max-width:520px){.account-list{grid-template-columns:1fr}.modal-foot{flex-wrap:wrap}.selection-count{flex-basis:100%}.account-row{min-height:44px}}
</style>
