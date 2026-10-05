<script setup>
import { computed } from 'vue'
import { displayTime } from './ai-format'
import { publicRequest, requestItems, requestTools, SCOPE_LABELS, toolAccess } from './request-context'

const props = defineProps({ contexts: { type: Array, default: () => [] }, expanded: { type: Boolean, default: false } })
const emit = defineEmits(['edit'])
const latest = computed(() => props.contexts.at(-1))
const earlier = computed(() => props.contexts.slice(0, -1))
const snapshots = computed(() => latest.value ? [latest.value] : [])
const json = value => JSON.stringify(publicRequest(value), null, 2)
</script>

<template>
  <details class="request-context" :open="expanded">
    <summary><span aria-hidden="true">≡</span><b>模型请求上下文</b><small>{{ SCOPE_LABELS[latest?.data?.scope] || '请求' }} · {{ contexts.length }} 轮 · {{ requestTools(latest?.data?.snapshot).length }} 个工具</small></summary>
    <div class="context-content">
      <div class="context-heading"><p>记录实际发送内容，按协议原始顺序展示。记忆、截图或其他上下文可能位于用户输入之后；图片仅保留元数据，凭据已脱敏。</p><button type="button" @click="emit('edit')">编辑基础提示词</button></div>
      <template v-for="entry in snapshots" :key="entry.id">
        <p class="tool-access">{{ toolAccess(entry.data?.snapshot,entry.data?.scope) }}</p>
        <p class="request-meta">{{ entry.data?.snapshot?.model }} · {{ entry.data?.snapshot?.protocol }} · {{ displayTime(entry.at) }}<span v-if="entry.data?.prompt_version"> · 提示词版本 {{ entry.data.prompt_version }}</span></p>
        <ol class="request-messages" aria-label="本轮模型输入顺序">
          <li v-for="block in requestItems(entry.data?.snapshot)" :key="block.index"><details :open="['system','developer'].includes(block.role)"><summary><span>{{ block.index }}.</span><b>{{ block.role }}</b><small v-if="block.item.request_field">{{ block.item.request_field }}</small></summary><pre>{{ block.text }}</pre></details></li>
        </ol>
        <details class="tool-directory"><summary>实际工具目录 · {{ requestTools(entry.data?.snapshot).length }} 个</summary><ul><li v-for="(tool,index) in requestTools(entry.data?.snapshot)" :key="`${index}:${tool.name}`"><details><summary>{{ tool.name }}</summary><pre>{{ json(tool.value) }}</pre></details></li></ul><p v-if="!requestTools(entry.data?.snapshot).length">工具目录为空。</p></details>
        <details class="raw-request"><summary>完整请求与脱敏记录</summary><pre>{{ json(entry.data) }}</pre></details>
      </template>
      <details v-if="earlier.length" class="earlier-context"><summary>此前 {{ earlier.length }} 轮请求</summary><details v-for="entry in earlier" :key="entry.id"><summary>{{ displayTime(entry.at) }} · {{ SCOPE_LABELS[entry.data?.scope] || '请求' }} · {{ requestTools(entry.data?.snapshot).length }} 个工具</summary><pre>{{ json(entry.data) }}</pre></details></details>
    </div>
  </details>
</template>

<style scoped>
.request-context{min-width:0;color:var(--text-2,#aab5b2);font-size:10px;border-bottom:1px solid var(--border,#41484a);padding-bottom:7px}.request-context>summary{display:flex;gap:7px;align-items:center;flex-wrap:wrap;list-style:none;cursor:pointer;padding:5px 0}.request-context>summary::-webkit-details-marker{display:none}.request-context>summary::after{content:'›';margin-left:auto;font-size:15px}.request-context[open]>summary::after{transform:rotate(90deg)}b{font-weight:550}small{font-size:9px}.context-content{display:grid;gap:9px;padding-top:7px;min-width:0}.context-heading{display:grid;gap:6px}.context-heading button{justify-self:start;border:0;background:transparent;color:var(--accent,#e4c956);font:inherit;cursor:pointer;padding:2px 0}.context-heading p,.request-meta{font-size:9px;line-height:1.7;margin:0;overflow-wrap:anywhere}.tool-access{margin:0;line-height:1.7;padding:7px 9px;background:var(--bg-2,#24282a);border-radius:5px}.request-messages{margin:0;padding:0;list-style:none;display:grid;gap:5px;min-width:0}.request-messages>li{border-left:1px solid var(--border,#41484a);padding-left:10px;min-width:0}.request-messages summary{display:flex;gap:7px;align-items:center;cursor:pointer;min-width:0}.request-messages summary>b{color:var(--text-0,#edf0ee)}pre{margin:6px 0 3px;padding:8px 9px;background:var(--bg-2,#24282a);white-space:pre-wrap;overflow-wrap:anywhere;word-break:break-word;border-radius:5px;font-size:10px;line-height:1.7;max-height:380px;overflow:auto;box-sizing:border-box;max-width:100%;user-select:text}.tool-directory,.raw-request,.earlier-context{min-width:0}.tool-directory>summary,.raw-request>summary,.earlier-context>summary{cursor:pointer;line-height:1.8}.tool-directory ul{list-style:none;padding:4px 0 0 9px;margin:0;display:grid;gap:5px}.tool-directory li{min-width:0}.tool-directory li summary,.earlier-context>details>summary{cursor:pointer;overflow-wrap:anywhere;line-height:1.8}.earlier-context>details{padding-left:10px;margin-top:7px}:focus-visible{outline:2px solid var(--accent,#e4c956);outline-offset:2px}
</style>
