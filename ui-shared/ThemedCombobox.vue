<template>
  <span ref="root" class="themed-combobox">
    <input ref="input" v-bind="$attrs" :value="modelValue" role="combobox" autocomplete="off"
      :aria-expanded="open" :aria-controls="listId" aria-autocomplete="list"
      :aria-activedescendant="open && active >= 0 ? `${listId}-${active}` : undefined"
      @input="update" @focus="show(false)" @blur="close" @keydown="keydown" />
    <button v-if="suggestionsEnabled" type="button" class="combo-toggle" :disabled="$attrs.disabled"
      :aria-label="`选择${$attrs['aria-label'] || '候选值'}`" :aria-expanded="open"
      @mousedown.prevent @click="toggle"><span class="menu-chevron" aria-hidden="true"></span></button>
    <Teleport to="body">
      <div v-if="open" :id="listId" ref="popup" class="combo-popup" :style="position" role="listbox" :aria-label="$attrs['aria-label'] || '候选值'">
        <div v-if="!filtered.length" class="combo-empty">无匹配项，可直接输入</div>
        <div v-for="(option, index) in filtered" :id="`${listId}-${index}`" :key="option.value"
          role="option" :aria-selected="option.value === modelValue" class="combo-option"
          :class="{ highlighted: index === active }" @mousedown.prevent @click="pick(option.value)" @mouseenter="active = index">
          <span>{{ option.label }}</span><small v-if="option.hint">{{ option.hint }}</small>
          <span v-if="option.value === modelValue" class="combo-check" aria-hidden="true">✓</span>
        </div>
      </div>
    </Teleport>
  </span>
</template>

<script>
let sequence = 0
const prefix = Math.random().toString(36).slice(2)
</script>
<script setup>
import { computed, nextTick, onBeforeUnmount, onDeactivated, ref, watch } from 'vue'
defineOptions({ inheritAttrs: false })
const props = defineProps({ modelValue: { type: String, default: '' }, options: { type: Array, default: () => [] }, suggestionsEnabled: { type: Boolean, default: true } })
const emit = defineEmits(['update:modelValue', 'commit'])
const listId = `gamer-combo-${prefix}-${++sequence}`
const root = ref(null), input = ref(null), popup = ref(null), open = ref(false), active = ref(-1), all = ref(false), position = ref({})
const filtered = computed(() => {
  const query = all.value ? '' : props.modelValue.toLocaleLowerCase()
  return props.options.map(option => typeof option === 'string' ? { value: option, label: option } : option)
    .filter(option => !query || `${option.value} ${option.label} ${option.hint || ''}`.toLocaleLowerCase().includes(query))
})
function place() {
  if (!root.value || !open.value) return
  const rect = root.value.getBoundingClientRect(), width = Math.min(Math.max(rect.width, 220), window.innerWidth - 16)
  const below = window.innerHeight - rect.bottom - 8, above = rect.top - 8
  const height = Math.min(260, Math.max(below, above))
  position.value = { left: `${Math.max(8, Math.min(rect.left, window.innerWidth - width - 8))}px`, width: `${width}px`, maxHeight: `${height}px`,
    ...(below >= Math.min(260, above) ? { top: `${rect.bottom + 4}px` } : { bottom: `${window.innerHeight - rect.top + 4}px` }) }
}
function show(showAll) {
  if (!props.suggestionsEnabled) return
  all.value = showAll; open.value = true; active.value = -1; place()
}
function close() { open.value = false; active.value = -1 }
function update(event) { emit('update:modelValue', event.target.value); show(false) }
function toggle() { const wasOpen = open.value; input.value?.focus(); if (wasOpen) close(); else show(true) }
function pick(value) { emit('update:modelValue', value); input.value?.focus(); close() }
function keydown(event) {
  if (event.defaultPrevented || !props.suggestionsEnabled || event.isComposing) return
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    event.preventDefault(); event.stopPropagation()
    if (!open.value) show(true)
    const count = filtered.value.length
    active.value = !count ? -1 : active.value < 0 ? (event.key === 'ArrowDown' ? 0 : count - 1)
      : (active.value + (event.key === 'ArrowDown' ? 1 : count - 1)) % count
    nextTick(() => popup.value?.querySelector('.highlighted')?.scrollIntoView?.({ block: 'nearest' }))
  } else if (event.key === 'Enter' && open.value && active.value >= 0) {
    event.preventDefault(); event.stopPropagation(); pick(filtered.value[active.value].value)
  } else if (event.key === 'Escape' && open.value) { event.preventDefault(); event.stopPropagation(); close() }
  else if (event.key === 'Tab') close()
  else if (event.key === 'Enter') { close(); emit('commit', event) }
}
function outside(event) { if (!root.value?.contains(event.target) && !popup.value?.contains(event.target)) close() }
function scrolled(event) { if (!popup.value?.contains(event.target)) close() }
watch(open, enabled => {
  const method = enabled ? 'addEventListener' : 'removeEventListener'
  window[method]('resize', place); document[method]('scroll', scrolled, true); document[method]('pointerdown', outside, true)
}, { flush: 'sync' })
watch(() => props.suggestionsEnabled, enabled => { if (!enabled) close() })
watch(filtered, () => { active.value = -1 }, { flush: 'sync' })
onBeforeUnmount(close)
onDeactivated(close)
defineExpose({ focus: () => input.value?.focus(), close })
</script>

<style scoped>
.themed-combobox{display:inline-flex;position:relative;align-items:stretch;min-width:0;max-width:100%;flex:1}
.themed-combobox>input{width:100%;min-width:0;min-height:28px;padding:3px 28px 3px 7px;border:1px solid var(--control-border);border-radius:var(--radius-sm);background:var(--field);color:var(--text-0);font-size:13px}
.themed-combobox>input:focus{outline:none;border-color:var(--accent)}
.combo-toggle{position:absolute;right:1px;top:1px;bottom:1px;width:25px;display:flex;align-items:center;justify-content:center;border:0;border-left:1px solid var(--border);border-radius:0 var(--radius-sm) var(--radius-sm) 0;background:var(--field);color:var(--text-2);cursor:pointer}
.combo-toggle:hover{color:var(--accent)}
.combo-popup{position:fixed;z-index:10000;overflow:auto;padding:4px;background:var(--bg-2);color:var(--text-0);border:1px solid var(--border);border-radius:var(--radius-sm);box-shadow:var(--shadow);font-size:13px}
.combo-option{display:flex;align-items:center;gap:10px;padding:6px 8px;border-radius:var(--radius-sm);cursor:pointer;overflow-wrap:anywhere}
.combo-option.highlighted,.combo-option:hover{background:var(--bg-3);color:var(--accent)}
.combo-option small{color:var(--text-2);font-size:12px}.combo-check{margin-left:auto;color:var(--accent)}
.combo-empty{padding:8px;color:var(--text-2);font-size:12px}
</style>
