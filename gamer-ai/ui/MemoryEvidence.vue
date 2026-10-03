<script setup>
import { computed } from 'vue'
const props = defineProps({ memory: { type: Object, required: true }, showCanonical: { type: Boolean, default: false } })
const validation = computed(() => props.memory.effective_validation || props.memory.validation)
const label = value => ({ pending: '待验证', verified: '来源已验证', invalid: '已失效' })[value] || value || '验证状态未知'
</script>
<template>
  <span class="memory-evidence">
    <span>{{ label(validation) }}</span>
    <small v-if="showCanonical && memory.validation && validation !== memory.validation">保存的验证标记：{{ label(memory.validation) }}；来源变化后当前需要重新验证，正文与保护字段保留。</small>
    <span v-if="memory.source_conflicts?.length" class="source-warning">
      <b>资料需复核</b>
      <small v-for="(source,index) in memory.source_conflicts" :key="`${source.source_id}:${index}`">来源 {{ source.source_id }} · 引用修订 {{ source.cited_revision == null ? '未注明' : `r${source.cited_revision}` }} → 当前 r{{ source.current_revision }}{{ source.deleted ? '（已删除）' : '（已修订）' }}</small>
      <small v-if="validation === 'verified'">其他独立来源仍支持当前内容，以上来源变更仍需检查。</small>
    </span>
  </span>
</template>
<style scoped>
.memory-evidence{display:grid;gap:4px;margin-top:4px;overflow-wrap:anywhere}small{display:block;line-height:1.7;color:var(--text-2,#aab5b2)}.source-warning{display:grid;gap:3px;color:var(--accent,#e4c956)}.source-warning small{color:inherit}
</style>
