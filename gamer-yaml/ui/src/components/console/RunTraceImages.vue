<script setup>
import { computed, ref, watch } from 'vue'
import { safeTraceImageUrl, traceBoxStyle, traceImageFilename } from './useRunTrace'
const props = defineProps({ runId: String, images: { type: Array, default: () => [] }, status: String, enabled: Boolean, gaps: { type: Array, default: () => [] } })
const selectedId = ref(''), broken = ref(new Set()), templateBroken = ref(false)
const selected = computed(() => props.images.find(image => image.image_id === selectedId.value))
const labels = { consumed: '实际观察帧', trace_on: '开启 Trace', trace_off: '关闭 Trace', error_last_consumed: '报错前最后观察帧', error_fresh: '报错后新截图', action_before: '操作前', action_after: '操作后' }
// Same-origin downloads carry the existing HttpOnly session cookie, like the
// original image request. No canvas encoding, blob rewriting, or fabricated media ID.
const canSaveFrame = computed(() => !!selected.value && ['available', 'retained'].includes(props.status) && !broken.value.has(selected.value.image_id))
const metadata = computed(() => selected.value?.metadata || {})
const region = computed(() => selected.value && traceBoxStyle(metadata.value.region, selected.value.width, selected.value.height))
const match = computed(() => selected.value && traceBoxStyle(metadata.value.match_box, selected.value.width, selected.value.height))
function unavailable(id) { broken.value = new Set([...broken.value, id]) }
watch(selectedId, () => { templateBroken.value = false })
watch(() => props.runId, () => { selectedId.value = ''; broken.value = new Set() })
watch(() => props.images, images => { if (!images.some(image => image.image_id === selectedId.value)) selectedId.value = images[0]?.image_id || '' }, { immediate: true })
</script>
<template>
  <section class="trace-images" aria-label="运行图像证据">
    <p v-if="status === 'expired'" class="warning">图像证据已过期，文字记录仍可查看</p>
    <p v-else-if="status === 'unavailable'" class="hint">本次运行没有可用图像证据</p>
    <p v-else-if="!enabled" class="hint">Trace 已关闭，仅展示已采集证据；空白时段不代表没有操作</p>
    <p v-for="(gap,index) in gaps" :key="index" class="warning">证据缺口：{{ gap.reason }}{{ gap.count ? ` · ${gap.count} 次` : '' }} · {{ gap.time }}</p>
    <div class="thumbnails"><button v-for="item in images" :key="item.image_id" class="thumbnail" :class="{ selected: item.image_id === selectedId }" :aria-label="`${labels[item.kind] || item.kind} ${item.captured_at}`" :aria-pressed="item.image_id === selectedId" @click="selectedId = item.image_id"><img v-if="!broken.has(item.image_id)" :src="safeTraceImageUrl(runId,item)" loading="lazy" alt="运行截图缩略图" @error="unavailable(item.image_id)" /><span v-else class="warning">图像不可用或已过期</span><small>{{ labels[item.kind] || item.kind }} · {{ item.seq }}</small></button></div>
    <template v-if="selected">
      <div v-if="!broken.has(selected.image_id)" class="image-stage"><img :src="safeTraceImageUrl(runId,selected)" alt="所选运行截图原图" @error="unavailable(selected.image_id)" /><span v-if="region" class="overlay region" :style="region" aria-label="搜索区域" /><span v-if="match" class="overlay match" :style="match" aria-label="模板命中框" /></div>
      <p v-else class="warning">当前图像无法读取，可能已过期。保留原始事件说明，不以其他图像代替。</p>
      <div class="frame-export"><a v-if="canSaveFrame" class="btn btn-sm" aria-label="保存原始画面" :href="safeTraceImageUrl(runId,selected)" :download="traceImageFilename(runId,selected)">保存画面</a><button v-else class="btn btn-sm" aria-label="保存原始画面" disabled>保存画面</button><span class="hint">保存原始 PNG 参考图，可供离线模板或回归检查；单张画面不包含操作与完成证据，不是完整回放素材</span></div>
      <p class="hint">{{ labels[selected.kind] || selected.kind }} · {{ selected.width }} × {{ selected.height }} · 采集 {{ selected.captured_at }}</p>
      <p class="hint">图像 ID {{ selected.image_id }}<span v-if="metadata.frame_id !== undefined"> · 调用栈帧 {{ metadata.frame_id }}</span></p>
      <p v-if="metadata.template">模板 {{ metadata.template }} · {{ metadata.found ? '命中' : '未命中' }}<span v-if="metadata.score !== undefined"> · 分数 {{ metadata.score }}</span><span v-if="metadata.threshold !== undefined"> · 阈值 {{ metadata.threshold }}</span></p>
      <details v-if="metadata.template_file"><summary>本次使用的模板图像</summary><img v-if="!templateBroken" class="template-image" :src="`${safeTraceImageUrl(runId,selected)}?template=true`" alt="运行快照中的模板" @error="templateBroken = true" /><p v-else class="warning">模板图像不可用或已过期</p></details>
      <p v-if="region || match" class="hint">黄色虚线：搜索区域 · 绿色实线：命中框</p><p v-if="metadata.error" class="warning">{{ metadata.error }}</p>
    </template>
    <p v-else-if="status === 'available'" class="hint">所选步骤没有关联图像。可在运行级“图像证据”查看其余截图。</p>
  </section>
</template>
<style scoped>
.frame-export{display:flex;gap:8px;align-items:flex-start;flex-wrap:wrap}.frame-export .btn{flex:none;text-decoration:none}.frame-export .hint{flex:1;min-width:160px;line-height:1.6}.template-image{max-width:100%;max-height:180px;object-fit:contain}.trace-images{display:flex;flex-direction:column;gap:7px;font-size:12px}.hint{color:var(--text-2);overflow-wrap:anywhere}.warning{color:var(--warn);overflow-wrap:anywhere}.thumbnails{display:flex;gap:6px;overflow:auto}.thumbnail{display:flex;flex-direction:column;gap:4px;flex:0 0 110px;padding:4px;background:var(--bg-0);border:1px solid var(--border);border-radius:4px;color:var(--text-1);cursor:pointer}.thumbnail.selected{border-color:var(--accent)}.thumbnail img{width:100%;height:72px;object-fit:contain}.thumbnail small{font-size:10px;line-height:1.4}.image-stage{position:relative;align-self:flex-start;width:100%;line-height:0}.image-stage>img{width:100%;height:auto;display:block}.overlay{position:absolute;pointer-events:none;box-sizing:border-box}.region{border:2px dashed #ffd25b}.match{border:2px solid #4efc91}:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
</style>
