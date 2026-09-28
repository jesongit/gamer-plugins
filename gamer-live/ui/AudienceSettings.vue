<script setup>
import { reactive, ref, watch } from 'vue'
import { api } from '../../../web/src/api'
const options = reactive({ layout: 'card', rows: 3, font: 22 }), busy = ref(false), error = ref(''), feedback = ref(''), url = ref('')
try { const saved = JSON.parse(localStorage.getItem('gamer-live-audience') || '{}'); for (const key of Object.keys(options)) if (saved[key] != null) options[key] = saved[key] } catch { /* Use defaults when local storage is unavailable. */ }
watch(options, value => { try { localStorage.setItem('gamer-live-audience', JSON.stringify(value)) } catch { /* Display preferences remain usable in memory. */ } })
async function open() {
  const popup = window.open('', 'gamer-live-audience', `popup,width=${options.layout === 'strip' ? 1000 : 440},height=${options.layout === 'strip' ? 300 : 760}`)
  if (popup) popup.opener = null
  busy.value = true; error.value = ''; feedback.value = ''
  try {
    const result = await api.callExtension('gamer-live', 'audience.open', {})
    const destination = new URL(result.url)
    destination.search = new URLSearchParams({ layout: options.layout, rows: options.rows, font: options.font }).toString()
    url.value = destination.href
    if (popup) { popup.location = url.value; feedback.value = '反馈窗口已打开，请在直播姬添加「窗口采集」并选择 Gamer 互动反馈。' }
    else feedback.value = '浏览器拦截了弹出窗口，请点击下方链接打开。'
  } catch (e) { error.value = e.message; popup?.close() } finally { busy.value = false }
}
async function close() {
  busy.value = true; error.value = ''
  try { await api.callExtension('gamer-live', 'audience.close', {}); url.value = ''; feedback.value = '上屏读取地址已撤销。再次打开会生成新地址，互动队列继续执行。' } catch (e) { error.value = e.message } finally { busy.value = false }
}
</script>
<template>
  <section aria-label="观众反馈窗口">
    <h3>观众反馈窗口</h3><p>让观众看到已入队、等待位置、正在执行和完成结果。</p>
    <div class="row"><label>布局<select v-model="options.layout"><option value="card">侧边队列卡</option><option value="strip">底部横条</option></select></label><label>等待项数<input v-model.number="options.rows" type="number" min="1" max="10" /></label><label>字号<input v-model.number="options.font" type="number" min="16" max="40" /></label></div>
    <div class="row"><button :disabled="busy" @click="open">打开 / 更新反馈窗口</button><button :disabled="busy" @click="close">停止上屏</button></div>
    <p class="hint">在运行 Gamer 服务的电脑上打开，用直播姬的「窗口采集」添加该窗口。保持窗口打开；关闭或停止上屏不影响互动执行。修改布局后重新打开生效。</p>
    <p class="hint">仅展示互动操作名与队列结果，不展示普通聊天、运行参数或技术错误。规则可单独设置公开显示名。</p>
    <a v-if="url" :href="url" target="_blank" rel="noopener noreferrer">手动打开反馈窗口</a><p v-if="feedback" role="status">{{ feedback }}</p><p v-if="error" class="error" role="alert">{{ error }}</p>
  </section>
</template>
<style scoped>
section{display:grid;gap:12px;border:1px solid var(--border,#41444c);border-radius:10px;padding:16px;font-size:13px;min-width:0}h3,p{margin:0}h3{font-size:14px}.row{display:flex;gap:10px;flex-wrap:wrap}label{display:grid;gap:6px;flex:1;min-width:80px}input,select,button{min-width:0;max-width:100%;box-sizing:border-box;background:var(--bg-1,#24262c);color:inherit;padding:7px;border:1px solid var(--border,#41444c);border-radius:5px}.hint{font-size:12px;opacity:.7;line-height:1.7}.error{color:#f19494}a{color:#77cbb4}
</style>
