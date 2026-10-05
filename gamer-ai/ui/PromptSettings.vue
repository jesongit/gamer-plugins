<script setup>
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import { api } from '../../../web/src/api'

const emit = defineEmits(['saved'])
const FIELDS = [
  { key:'chat_system_prompt',label:'对话基础系统提示词',description:'普通问答、攻略查询，以及用户委托的记忆维护。' },
  { key:'game_system_prompt',label:'游玩基础系统提示词',description:'AI 观察游戏画面、规划并调用设备工具。' },
  { key:'import_system_prompt',label:'记忆整理基础系统提示词',description:'后台将攻略和游玩经验合并为可查询记忆。' },
]
const version = ref(null), loaded = ref(false), busy = ref(false), error = ref(''), feedback = ref('')
const values = reactive(Object.fromEntries(FIELDS.map(field=>[field.key,''])))
const defaults = reactive({}), baseline = reactive({})
const bytes = value => new TextEncoder().encode(value || '').length
const valid = computed(() => FIELDS.every(field => values[field.key].trim() && bytes(values[field.key]) <= 32768))
const changed = computed(() => FIELDS.some(field => values[field.key] !== baseline[field.key]))
let disposed = false
const call = (action, data = {}) => api.callExtension('gamer-ai', action, data)
function apply(value) {
  version.value = value.version ?? null
  for (const field of FIELDS) { values[field.key] = value[field.key] || ''; baseline[field.key] = values[field.key]; defaults[field.key] = value.defaults?.[field.key] || '' }
  loaded.value = true
}
async function load() {
  busy.value = true; error.value = ''; feedback.value = ''
  try { const result = await call('prompts.get'); if (!disposed) apply(result) }
  catch (e) { if (!disposed) error.value = e.message || '无法读取基础提示词' }
  finally { if (!disposed) busy.value = false }
}
function restore(key) { values[key] = defaults[key] || ''; feedback.value = '已填入默认提示词，点击保存后生效。' }
async function save() {
  if (!loaded.value || !valid.value || busy.value) return
  busy.value = true; error.value = ''; feedback.value = ''
  try {
    const result = await call('prompts.save', { expected_version:version.value,...values })
    if (!disposed) { apply(result); feedback.value = '基础提示词已保存，将用于下一次模型请求。运行中的请求及历史快照保留原内容。'; emit('saved') }
  } catch (e) { if (!disposed) error.value = e.message || '保存基础提示词失败' }
  finally { if (!disposed) busy.value = false }
}
onMounted(load)
onBeforeUnmount(() => { disposed = true })
</script>

<template>
  <section class="prompt-settings" aria-label="基础系统提示词设置">
    <header><h3>系统提示词</h3><button type="button" :disabled="busy" @click="load">重新读取</button></header>
    <p>三种请求分别配置，账号级保存。每轮实际发送的系统提示词、历史、记忆、图片元数据与工具目录，会记录在对话的「模型请求上下文」。</p>
    <p class="readonly-context">动态权限、设备状态、坐标约束、检索结果和导入来源由运行时注入，不能通过修改基础提示词取消。实际内容可在请求快照中只读查看。</p>
    <p v-if="error" role="alert" class="error">{{ error }}</p><p v-if="feedback" role="status" class="feedback">{{ feedback }}</p>
    <form @submit.prevent="save"><fieldset :disabled="busy || !loaded">
      <section v-for="field in FIELDS" :key="field.key" class="prompt-field"><div class="field-heading"><label :for="`prompt-${field.key}`">{{ field.label }}</label><button type="button" @click="restore(field.key)">恢复默认</button></div><small>{{ field.description }} {{ values[field.key]===defaults[field.key] ? '· 使用默认' : '· 已自定义' }}</small><textarea :id="`prompt-${field.key}`" v-model="values[field.key]" :aria-label="field.label" rows="10" spellcheck="false" /><small :class="{error:bytes(values[field.key])>32768}">{{ bytes(values[field.key]).toLocaleString('zh-CN') }} / 32,768 UTF-8 字节</small></section>
      <p v-if="!valid" class="error">提示词不能为空或仅含空白；每份最多 32 KiB，请修正对应字段。</p>
      <button class="save-button" type="submit" :disabled="!valid || !changed">{{ busy ? '正在保存…' : '保存提示词' }}</button>
    </fieldset></form>
  </section>
</template>

<style scoped>
.prompt-settings{display:grid;gap:10px;font-size:11px;min-width:0}.prompt-settings>header{display:flex;gap:8px;justify-content:space-between;align-items:center}h3,p{margin:0}h3{font-size:13px}p{line-height:1.8;color:var(--text-2,#aab5b2);overflow-wrap:anywhere}.readonly-context{font-size:10px;padding:8px;background:var(--bg-1,#181b1c);border-radius:5px}form,fieldset{display:grid;gap:15px;margin:0;padding:0;border:0;min-width:0}.prompt-field{display:grid;gap:5px;min-width:0}.field-heading{display:flex;align-items:center;justify-content:space-between;gap:8px;flex-wrap:wrap}.field-heading label{font-weight:550}small{font-size:10px;color:var(--text-2,#aab5b2);line-height:1.7}textarea{display:block;box-sizing:border-box;resize:vertical;min-height:150px;width:100%;max-width:100%;padding:9px;border:1px solid var(--border,#41484a);border-radius:5px;background:var(--bg-1,#181b1c);color:inherit;font:inherit;line-height:1.8;white-space:pre-wrap}button{font:inherit;padding:4px 7px;border:1px solid var(--border,#41484a);border-radius:5px;background:var(--bg-1,#181b1c);color:inherit;cursor:pointer}.field-heading button{font-size:10px;border:0;background:transparent;color:var(--accent,#e4c956);padding:2px 0}.save-button{justify-self:start;color:var(--accent,#e4c956)}.error{color:var(--danger,#ef9292)}.feedback{color:#77cbb4}button:disabled,fieldset:disabled{opacity:.5;cursor:default}:focus-visible{outline:2px solid var(--accent,#e4c956);outline-offset:2px}
</style>
