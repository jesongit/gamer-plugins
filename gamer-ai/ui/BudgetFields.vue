<script setup>
import {LIMIT_FIELDS} from './budget-format'
const props=defineProps({modelValue:{type:Object,required:true},prefix:{type:String,default:''}})
const emit=defineEmits(['update:modelValue'])
function update(key,event){const raw=event.target.value;emit('update:modelValue',{...props.modelValue,[key]:raw===''?'':Number(raw)})}
</script>
<template>
  <div class="budget-grid"><label v-for="field in LIMIT_FIELDS" :key="field.key">{{ field.label }}（0 = 无限）<input :value="modelValue[field.key]" type="number" min="0" :max="field.max" :aria-label="`${prefix}${field.label}预算`" @input="update(field.key,$event)" /></label></div>
</template>
<style scoped>
.budget-grid{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:8px}label{display:grid;gap:5px;font-size:11px}input{font:inherit;min-width:0;color:inherit;background:var(--bg-1,#181b1c);border:1px solid var(--border,#41484a);border-radius:4px;padding:6px}input:focus-visible{outline:2px solid var(--accent,#e4c956);outline-offset:2px}
</style>
