<script setup>
import { ref, onMounted } from 'vue'
import { api } from '../../../web/src/api'
const props = defineProps({ payload: { type: Object, default: () => ({}) }, entrypoint: String })
const emit = defineEmits(['update:payload'])
const profiles = ref([]), error = ref('')
const update = (key, value) => emit('update:payload', { ...props.payload, [key]: value })
onMounted(async () => { try { profiles.value = (await api.callExtension('gamer-ai', 'settings.read', {})).profiles } catch (e) { error.value = e.message } })
defineExpose({ validate: () => props.entrypoint?.endsWith('#goal') && !props.payload.goal?.trim() ? [{ name: 'goal', message: '请输入自然语言目标' }] : [] })
</script>
<template>
  <div class="ai-goal-editor">
    <label>目标<textarea :value="payload.goal || ''" rows="3" class="input" placeholder="例如：完成今天的日常" @input="update('goal', $event.target.value)" /></label>
    <label>多模态模型<select class="select" :value="payload.model_profile_id || ''" @change="update('model_profile_id', $event.target.value)"><option value="">使用第一个已配置模型</option><option v-for="p in profiles" :key="p.id" :value="p.id">{{ p.id }} · {{ p.model }} · {{ p.vision }}</option></select></label>
    <p>后台按目标执行。体力正常使用；道具与货币须授权。待授权映射为任务“失败”，具体进度在 AI 会话中查看。新任务默认由 AI 发送结果；启用对应任务通知规则后由任务通知发送。</p>
    <p v-if="error" role="alert">{{ error }}</p>
  </div>
</template>
<style scoped>.ai-goal-editor{display:grid;gap:8px}.ai-goal-editor label{display:grid;gap:4px}.ai-goal-editor p{font-size:12px;color:var(--text-secondary)}</style>
