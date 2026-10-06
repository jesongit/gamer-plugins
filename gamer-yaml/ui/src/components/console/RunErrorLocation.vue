<template>
  <Teleport to="body"><div class="modal-mask" v-backdrop-dismiss="() => $emit('close')"><section class="modal error-location">
    <div class="modal-head"><span>运行报错位置</span><button class="btn btn-icon" aria-label="关闭报错位置" @click="$emit('close')"><UiIcon name="close" /></button></div>
    <div class="modal-body">
      <p class="source mono">{{ source?.package_id }} / {{ source?.path }}<template v-if="source?.function"> · {{ source.function }}</template></p>
      <p class="source">{{ event.path }} · 调用帧 {{ event.trace?.frame_id }} · {{ event.trace?.run_id }}</p>
      <p class="failure">{{ event.error || '运行步骤' }}</p>
      <p v-if="note" role="status">{{ note }}</p>
      <pre v-if="sourceText !== null" class="source-yaml" aria-label="运行版本源码">{{ sourceText }}</pre>
    </div>
    <div class="modal-foot"><span>只读源码与步骤路径，当前未保存修改已保留</span><button class="btn" @click="$emit('close')">关闭</button></div>
  </section></div></Teleport>
</template>
<script setup>
import { vBackdropDismiss } from '../../../../../ui-shared/backdrop-dismiss.js'
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import UiIcon from '../../../../../../web/src/components/ui/UiIcon.vue'
import { api } from '../../../../../../web/src/api'
const props = defineProps({ event: { type: Object, required: true } })
defineEmits(['close'])
const source = computed(() => props.event.trace?.source)
const note = ref('正在读取定义…'), sourceText = ref(null)
let generation = 0
watch(() => props.event, async () => {
  const request = ++generation, s = source.value, snapshot = props.event.source_snapshot
  sourceText.value = null; note.value = '正在读取定义…'
  if (!s?.package_id || !s.path?.startsWith('automations/')) { note.value = '该事件没有可验证的资源位置。'; return }
  const frozen = [snapshot?.trace?.entry, ...Object.values(snapshot?.trace?.functions || {})].find(item => item?.package_id === s.package_id && item?.path === s.path && item?.version === s.version)
  const text = snapshot?._source_files?.[s.path]
  if (frozen && snapshot.trace.run_id === props.event.trace?.run_id && typeof text === 'string') {
    sourceText.value = text; note.value = `执行时冻结版本 ${s.version}，当前磁盘修改不影响这份证据`; return
  }
  try {
    const id = `${s.package_id}/${s.path.slice('automations/'.length)}`
    const file = await (s.function ? api.getFunction(id) : api.getScript(id))
    if (request !== generation) return
    if (!s.version || file.version !== s.version) { note.value = '源文件已变化，无法把执行时步骤对应到当前内容。请根据上方文件、函数和路径检查；当前编辑未改变。'; return }
    sourceText.value = file.content ?? ''; note.value = `已核对执行版本 ${s.version}`
  } catch (error) { if (request === generation) note.value = `无法读取执行源文件：${error.message}` }
}, { immediate: true })
onBeforeUnmount(() => { generation++ })
</script>
<style scoped>
.error-location{width:min(740px,calc(100vw - 32px))}.modal-body{max-height:70vh;overflow:auto}.source{font-size:12px;overflow-wrap:anywhere;margin:4px 0}.failure{color:var(--danger);white-space:pre-wrap}.modal-foot span{font-size:12px;color:var(--text-2);margin-right:auto}.source-yaml{font:12px/1.7 var(--mono,monospace);background:var(--bg-0);border:1px solid var(--border);padding:12px;white-space:pre-wrap;overflow-wrap:anywhere;tab-size:2}
</style>
