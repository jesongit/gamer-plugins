<script setup>
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import { api } from '../../../web/src/api'

const version = ref(null), busy = ref(false), error = ref(''), feedback = ref('')
const keys = reactive({ embedding: '', search: '' }), clearKeys = reactive({ embedding: false, search: false })
const hasKeys = reactive({ embedding: false, search: false })
const embedding = reactive({ enabled: false, provider: 'disabled', protocol: 'openai_embeddings', base_url: '', endpoint: '', model: '', account_id: '', request_timeout_secs: 30, max_input_bytes: 480, document_prefix: '', query_prefix: '' })
const search = reactive({ enabled: false, provider: 'disabled', protocol: 'custom', base_url: '', endpoint: '', request_timeout_secs: 30, max_results: 5 })
const webRead = reactive({ enabled: false, request_timeout_secs: 30, max_bytes: 262144, allowed_hosts: [], allow_private_networks: false })
const hosts = ref('')
const prefixBytes = value => new TextEncoder().encode(value || '').length
const embeddingOptionsValid = computed(() => Number.isInteger(embedding.max_input_bytes) && embedding.max_input_bytes >= 128 && embedding.max_input_bytes <= 24576 && [embedding.document_prefix,embedding.query_prefix].every(prefix => prefixBytes(prefix) <= 256 && prefixBytes(prefix) < embedding.max_input_bytes))
let disposed = false
function apply(value) {
  version.value = value.version ?? null
  for (const [name, target] of [['embedding', embedding], ['search', search]]) {
    const config = value[name] || {}
    for (const key of Object.keys(target)) if (config[key] != null) target[key] = config[key]
    hasKeys[name] = !!config.has_key
    keys[name] = ''; clearKeys[name] = false
  }
  embedding.max_input_bytes = value.embedding?.max_input_bytes ?? 480
  embedding.document_prefix = value.embedding?.document_prefix ?? ''
  embedding.query_prefix = value.embedding?.query_prefix ?? ''
  for (const key of Object.keys(webRead)) if (value.web_read?.[key] != null) webRead[key] = value.web_read[key]
  hosts.value = webRead.allowed_hosts.join('\n')
}
async function load() {
  busy.value = true; error.value = ''
  try { const value = await api.callExtension('gamer-ai', 'services.get', {}); if (!disposed) apply(value) }
  catch (e) { if (!disposed) error.value = e.message || '无法读取可选服务' }
  finally { if (!disposed) busy.value = false }
}
function preset(name) {
  if (name === 'embedding') {
    if (embedding.provider === 'cloudflare') {
      embedding.protocol = 'cloudflare'; embedding.base_url = 'https://api.cloudflare.com/client/v4'
      embedding.model = '@cf/baai/bge-m3'; embedding.endpoint = ''
    } else if (embedding.provider === 'openai') {
      embedding.protocol = 'openai_embeddings'; embedding.base_url = 'https://api.openai.com/v1'
      embedding.model = 'text-embedding-3-small'; embedding.endpoint = ''
    }
  } else {
    const presets = { tavily: ['tavily', 'https://api.tavily.com'], brave: ['brave', 'https://api.search.brave.com/res/v1'], searxng: ['searxng', ''] }
    if (presets[search.provider]) [search.protocol, search.base_url] = presets[search.provider]
    search.endpoint = ''
  }
}
async function save() {
  if (!embeddingOptionsValid.value) { error.value = '编码字节上限须为 128–24576 的整数，前缀各不超过 256 个 UTF-8 字节，且小于编码上限。'; return }
  busy.value = true; error.value = ''; feedback.value = ''
  try {
    const values = { expected_version: version.value,
      embedding: { ...embedding, api_key: keys.embedding, clear_key: clearKeys.embedding },
      search: { ...search, api_key: keys.search, clear_key: clearKeys.search },
      web_read: { ...webRead, allowed_hosts: hosts.value.split(/[,\n]/).map(item => item.trim()).filter(Boolean) } }
    const value = await api.callExtension('gamer-ai', 'services.save', values)
    if (!disposed) { apply(value); feedback.value = '可选服务已保存。聊天、向量与搜索分别配置，保存不会发起付费请求。' }
  } catch (e) { if (!disposed) error.value = e.message || '保存服务失败' }
  finally { if (!disposed) busy.value = false }
}
onMounted(load)
onBeforeUnmount(() => { disposed = true; keys.embedding = ''; keys.search = '' })
</script>

<template>
  <section class="services-panel">
    <h3>可选服务</h3>
    <p>不配置也可以聊天和使用关键词记忆检索。向量、联网搜索与网页读取各自启用；不会自动切换供应商或开通收费服务。</p>
    <p v-if="error" role="alert" class="error">{{ error }}</p><p v-if="feedback" role="status">{{ feedback }}</p>
    <form autocomplete="off" @submit.prevent="save"><fieldset :disabled="busy">
      <section><h4>向量检索</h4>
        <label class="check"><input v-model="embedding.enabled" type="checkbox" />启用独立 embedding 服务</label>
        <label>提供方<select v-model="embedding.provider" aria-label="向量服务提供方" @change="preset('embedding')"><option value="disabled">未配置</option><option value="cloudflare">Cloudflare Workers AI</option><option value="openai">OpenAI</option><option value="custom">自定义兼容服务</option></select></label>
        <label>协议<select v-model="embedding.protocol"><option value="openai_embeddings">OpenAI Embeddings</option><option value="cloudflare">Cloudflare Workers AI</option></select></label>
        <label>基础地址<input v-model="embedding.base_url" aria-label="向量服务基础地址" type="url" /></label>
        <label>完整接口地址（可选，优先于基础地址）<input v-model="embedding.endpoint" type="url" /></label>
        <label>Embedding 模型<input v-model="embedding.model" aria-label="Embedding 模型" /></label>
        <label>单次编码字节上限<input v-model.number="embedding.max_input_bytes" aria-label="单次编码字节上限" type="number" min="128" max="24576" step="1" required /></label>
        <small>保守约束模型 Token 边界；上限包含标题与编码前缀。480 字节是小模型的保守值，范围 128–24576。</small>
        <label>文档前缀（可选）<input v-model="embedding.document_prefix" aria-label="文档编码前缀" maxlength="256" placeholder="例如：passage: " /><small>{{ prefixBytes(embedding.document_prefix) }} / 256 UTF-8 字节</small></label>
        <label>查询前缀（可选）<input v-model="embedding.query_prefix" aria-label="查询编码前缀" maxlength="256" placeholder="例如：query: " /><small>{{ prefixBytes(embedding.query_prefix) }} / 256 UTF-8 字节</small></label>
        <small>前缀字节数须小于编码上限。编码上限和前缀变化会改变向量指纹，请到记忆库重建语义索引；同一端点调整这些参数不更换密钥。</small>
        <p v-if="!embeddingOptionsValid" class="error">请填写合法的编码上限，两个前缀各最多 256 个 UTF-8 字节，且小于编码上限。</p>
        <label v-if="embedding.protocol === 'cloudflare'">Cloudflare Account ID<input v-model="embedding.account_id" aria-label="Cloudflare Account ID" /></label>
        <label>独立密钥<input v-model="keys.embedding" type="password" aria-label="向量服务密钥" autocomplete="new-password" :disabled="clearKeys.embedding" :placeholder="hasKeys.embedding ? '已保存；留空保留' : '尚未配置'" /></label>
        <label class="check"><input v-model="clearKeys.embedding" type="checkbox" @change="keys.embedding = ''" />清除已保存的向量密钥</label>
        <label>请求超时（秒）<input v-model.number="embedding.request_timeout_secs" type="number" min="5" max="300" /></label>
        <small>本地保存派生向量；模型变化需要重建向量。提供方额度和实际用量独立于聊天 Token。</small>
      </section>
      <section><h4>独立联网搜索</h4>
        <label class="check"><input v-model="search.enabled" type="checkbox" />启用搜索服务</label>
        <label>提供方<select v-model="search.provider" aria-label="搜索服务提供方" @change="preset('search')"><option value="disabled">未配置</option><option value="tavily">Tavily</option><option value="brave">Brave Search</option><option value="searxng">自建 SearXNG</option><option value="custom">自定义兼容服务</option></select></label>
        <label>协议<select v-model="search.protocol"><option value="tavily">Tavily</option><option value="brave">Brave</option><option value="searxng">SearXNG</option><option value="custom">自定义</option></select></label>
        <label>基础地址<input v-model="search.base_url" aria-label="搜索服务基础地址" type="url" /></label>
        <label>完整接口地址（可选）<input v-model="search.endpoint" type="url" /></label>
        <label>独立密钥<input v-model="keys.search" type="password" aria-label="搜索服务密钥" autocomplete="new-password" :disabled="clearKeys.search" :placeholder="hasKeys.search ? '已保存；留空保留' : '尚未配置'" /></label>
        <label class="check"><input v-model="clearKeys.search" type="checkbox" @change="keys.search = ''" />清除已保存的搜索密钥</label>
        <label>最多返回条数<input v-model.number="search.max_results" type="number" min="1" max="10" /></label>
        <label>请求超时（秒）<input v-model.number="search.request_timeout_secs" type="number" min="5" max="300" /></label>
        <small>搜索与阅读结果会消耗各自服务额度，聊天模型阅读结果仍会使用 Token。缺少计费回执时用量显示未知。</small>
      </section>
      <section><h4>受控网页读取</h4>
        <label class="check"><input v-model="webRead.enabled" type="checkbox" />启用网页读取</label>
        <label>允许的域名（每行一个；留空允许公网）<textarea v-model="hosts" aria-label="网页读取允许域名" rows="3" /></label>
        <label class="check"><input v-model="webRead.allow_private_networks" type="checkbox" />明确允许本机及私有网络</label>
        <label>正文大小上限（字节）<input v-model.number="webRead.max_bytes" type="number" min="1024" max="4194304" /></label>
        <label>请求超时（秒）<input v-model.number="webRead.request_timeout_secs" type="number" min="5" max="300" /></label>
        <small>默认阻止私有网络；不跟随跳转，不携带聊天或搜索密钥。网页内容作为参考资料处理。</small>
      </section>
      <button type="submit" :disabled="busy || !embeddingOptionsValid">{{ busy ? '正在保存…' : '保存可选服务' }}</button>
    </fieldset></form>
  </section>
</template>

<style scoped>
.services-panel{padding:16px;overflow:auto;min-height:0;display:grid;gap:12px;font-size:12px}h3,h4,p{margin:0}p,small{line-height:1.7;color:var(--text-2,#aab5b2)}form,fieldset,section section{display:grid;gap:12px}fieldset{border:0;margin:0;padding:0}section section{padding:12px;border:1px solid var(--border,#41484a);border-radius:7px}label{display:grid;gap:5px}.check{display:flex;align-items:center;gap:8px}.check input{width:auto}input,textarea,select,button{font:inherit;color:inherit;background:var(--bg-1,#181b1c);border:1px solid var(--border,#41484a);border-radius:4px;padding:7px;min-width:0}button{justify-self:start;cursor:pointer}button:disabled,fieldset:disabled{opacity:.5}.error{color:var(--danger,#ef9292)}:focus-visible{outline:2px solid var(--accent,#e4c956);outline-offset:2px}
</style>
