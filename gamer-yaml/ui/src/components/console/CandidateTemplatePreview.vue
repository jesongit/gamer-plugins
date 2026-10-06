<script setup>
import { onBeforeUnmount, ref, watch } from 'vue'
import { api } from '../../../../../../web/src/api'
const props = defineProps({ packageId: String, candidateId: String, revision: Number, proposalId: { type: String, default: '' }, name: String })
const image = ref(null), loading = ref(false), error = ref('')
let generation = 0
async function preview() {
  const request = ++generation
  loading.value = true; error.value = ''
  try {
    const result = await api.callExtension('gamer-yaml', 'generation.template', { package_id: props.packageId, candidate_id: props.candidateId, name: props.name, ...(props.proposalId ? { pending: true } : {}) })
    if (request !== generation) return
    if (result.name !== props.name || result.mime_type !== 'image/png' || typeof result.base64 !== 'string' || result.base64.length > 32 * 1024 * 1024 || !/^[A-Za-z0-9+/]+={0,2}$/.test(result.base64) || !(result.width > 0 && result.height > 0)) throw new Error('模板图像响应不完整或身份不匹配')
    image.value = result
  } catch (e) { if (request === generation) error.value = e.message }
  finally { if (request === generation) loading.value = false }
}
watch(() => [props.packageId, props.candidateId, props.revision, props.proposalId, props.name], () => { generation++; loading.value = false; image.value = null; error.value = '' })
onBeforeUnmount(() => { generation++ })
</script>
<template>
  <div class="template-preview"><button class="btn btn-sm" :disabled="loading" @click="preview">{{ loading ? '读取中…' : '查看实际模板' }}</button><p v-if="error" role="alert">{{ error }}</p><figure v-if="image"><img :src="`data:image/png;base64,${image.base64}`" :alt="`候选模板 ${name}`" @error="error = '模板图像无法解码'; image = null" /><figcaption>{{ name }} · {{ image.width }} × {{ image.height }}</figcaption></figure></div>
</template>
<style scoped>
.template-preview{margin:5px 0}.template-preview p{color:var(--danger);overflow-wrap:anywhere}figure{margin:6px 0;padding:6px;background:var(--bg-0);border:1px solid var(--border);border-radius:4px}img{max-width:100%;max-height:220px;object-fit:contain}figcaption{font-size:11px;color:var(--text-2)}
</style>
