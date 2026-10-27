<script setup>
import { computed, onMounted, ref } from 'vue'
import { api } from '../../../web/src/api'

const enabled = ref(false), url = ref(''), text = ref(''), savedText = ref(''), version = ref(null)
const loading = ref(true), busy = ref(false), error = ref(''), feedback = ref(''), loaded = ref(false)
const dirty = computed(() => text.value !== savedText.value)
const characterCount = computed(() => Array.from(text.value).length)
function apply(result, replaceText = false) {
  enabled.value = result.enabled === true
  url.value = result.url || ''
  savedText.value = result.text || ''
  version.value = result.version ?? null
  if (replaceText) text.value = savedText.value
}
async function load() {
  loading.value = true; error.value = ''
  try { apply(await api.callExtension('gamer-live', 'audience.read', {}), true); loaded.value = true }
  catch (e) { error.value = `无法读取观众反馈设置：${e.message}` }
  finally { loading.value = false }
}
async function toggle(event) {
  const next = event.target.checked
  event.target.checked = enabled.value
  busy.value = true; error.value = ''; feedback.value = ''
  try {
    apply(await api.callExtension('gamer-live', next ? 'audience.open' : 'audience.close', {}))
    feedback.value = enabled.value ? '网页服务已启用，可复制 URL 添加到直播姬的浏览器／网页素材。' : '网页服务已关闭，互动队列继续执行。'
  } catch (e) { error.value = `切换未完成：${e.message}` }
  finally { busy.value = false }
}
async function save() {
  busy.value = true; error.value = ''; feedback.value = ''
  try {
    apply(await api.callExtension('gamer-live', 'audience.save', { text: text.value, expected_version: version.value }), true)
    feedback.value = '提示文字已保存，观众页会自动更新。'
  } catch (e) { error.value = `文字未保存：${e.message}` }
  finally { busy.value = false }
}
async function copy() {
  error.value = ''; feedback.value = ''
  try { await navigator.clipboard.writeText(url.value); feedback.value = '地址已复制。' }
  catch { error.value = '复制未完成，请选中地址后手动复制。' }
}
onMounted(load)
</script>

<template>
  <section class="audience-settings" aria-label="观众反馈">
    <div class="audience-heading">
      <h3>观众反馈</h3>
      <label class="enable-control"><span>{{ enabled ? '已启用' : '启用' }}</span><input type="checkbox" role="switch" aria-label="启用观众反馈" :aria-checked="enabled" :checked="enabled" :disabled="loading || busy || !loaded" @change="toggle" /></label>
    </div>
    <p v-if="loading" class="hint" role="status">正在读取设置…</p>
    <template v-if="loaded">
      <div v-if="url" class="audience-address">
        <label for="audience-url">网页素材地址</label>
        <input id="audience-url" :value="url" readonly spellcheck="false" @focus="$event.target.select()" />
        <div class="address-actions"><button type="button" @click="copy">复制地址</button></div>
        <p class="hint address-hint">同机直播姬 → 网页素材。关闭不会停止互动。</p>
      </div>
      <p v-else class="hint">启用后网页可读取。关闭不会停止互动任务。</p>
      <form class="audience-text" @submit.prevent="save">
        <label for="audience-text">底部提示文字<span class="optional">可留空</span></label>
        <textarea id="audience-text" v-model="text" rows="2" :disabled="busy" :aria-invalid="characterCount > 1000" spellcheck="false" aria-describedby="audience-text-count" />
        <div class="text-actions"><span id="audience-text-count" class="hint" :class="{ error: characterCount > 1000 }">{{ characterCount }} / 1000</span><button type="submit" :disabled="loading || busy || !dirty || characterCount > 1000">{{ busy ? '处理中…' : '保存文字' }}</button></div>
      </form>
    </template>
    <p v-if="feedback" class="feedback" role="status">{{ feedback }}</p>
    <p v-if="error" class="error" role="alert">{{ error }}</p>
    <button v-if="!loaded && !loading" type="button" @click="load">重新读取</button>
  </section>
</template>

<style scoped>
.audience-settings{display:grid;gap:10px;min-width:0;padding:10px;border:1px solid var(--border,#3b3d42);border-radius:4px;color:var(--text-0,#eeece6);font-size:13px}
.audience-heading{display:flex;align-items:center;justify-content:space-between;gap:10px}.audience-heading>div{min-width:0}h3,p{margin:0}h3{font-size:14px;font-weight:600;margin-bottom:0}.hint{font-size:12px;line-height:1.6;color:var(--text-2,#b6b6ae)}
.enable-control{display:flex;align-items:center;gap:8px;flex-shrink:0;font-size:12px}.enable-control input{accent-color:var(--accent,#dda951);width:32px;height:22px;margin:0;cursor:pointer}
.audience-address,.audience-text{display:grid;gap:8px;min-width:0}label{font-weight:500}.optional{margin-left:8px;font-weight:400;color:var(--text-2,#b6b6ae);font-size:12px}
input,textarea,button{box-sizing:border-box;min-width:0;max-width:100%;border:1px solid var(--border,#3b3d42);border-radius:3px;font:inherit;color:inherit}input[readonly],textarea{width:100%;background:var(--field,#191b1f);padding:6px 8px;line-height:1.6}input[readonly]{font-size:12px;color:var(--text-1,#cecac1)}textarea{resize:vertical;min-height:56px;caret-color:var(--accent,#dda951)}textarea::placeholder{color:var(--text-2,#b6b6ae)}
.address-actions,.text-actions{display:flex;align-items:center;gap:14px}.text-actions{justify-content:space-between}button{padding:6px 10px;background:var(--bg-3,#34363b);cursor:pointer;font-size:12px;justify-self:start}button:hover:not(:disabled){background:var(--border,#3b3d42)}button:active:not(:disabled){background:var(--bg-2,#282a2f)}button:disabled,input:disabled,textarea:disabled{opacity:.55;cursor:default}a{color:var(--accent-2,#edbc6c);font-size:12px;text-underline-offset:3px}a:hover{color:var(--text-0,#eeece6)}:is(input,textarea,button,a):focus-visible{outline:2px solid var(--accent,#dda951);outline-offset:3px}.feedback{color:var(--ok,#8fc9ae);line-height:1.6}.error{color:var(--danger,#ed9696);line-height:1.6;overflow-wrap:anywhere}::selection{background:var(--accent,#dda951);color:var(--accent-ink,#201d16)}
@media(max-width:360px){.audience-settings{padding:12px;gap:14px}.audience-heading{gap:10px;align-items:flex-start}}
.audience-address{grid-template-columns:minmax(0,1fr) auto;align-items:center}.audience-address>label,.address-hint{grid-column:1/-1}
</style>
