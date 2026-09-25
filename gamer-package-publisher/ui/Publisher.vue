<template>
  <section class="publisher">
    <h3>发布配置包</h3>
    <p>将本地配置发布到自己的公开 GitHub 仓库。已导出的包可先在“配置包”页导入。</p>
    <div class="actions"><button class="btn btn-sm" :disabled="!!busy" @click="checkLogin">检查 gh 登录</button><span>{{ account ? `当前账号：${account}` : '使用运行 Gamer 的电脑上的 gh 登录' }}</span></div>
    <label>目标仓库<input v-model="repository" class="input" placeholder="https://github.com/owner/repo" :disabled="!!busy" /></label>
    <fieldset :disabled="!!busy"><legend>选择本次新增或更新的配置（其他已发布配置自动保留）</legend>
      <p v-if="!packages.length">没有本地配置，请先创建或导入。</p>
      <label v-for="p in packages" :key="p.id" class="check"><input v-model="selected" type="checkbox" :value="p.id" />{{ p.name || p.id }} · {{ p.id }} · v{{ p.version }}</label>
      <label class="check"><input v-model="includeMedia" type="checkbox" />附带引用的媒体文件</label>
    </fieldset>
    <label>发布说明<textarea v-model="notes" class="input" rows="3" :disabled="!!busy" /></label>
    <div class="actions"><button class="btn btn-primary" :disabled="busy || !selected.length || !repository.trim()" @click="prepare">生成发布预览</button><button v-if="busy" class="btn" @click="cancel">取消操作</button></div>
    <p v-if="busy" role="status">{{ busy }}。正在处理完整目录，请稍候…</p>
    <p v-if="error" role="alert" class="error">{{ error }}</p>
    <p v-if="message" role="status">{{ message }}</p>
    <div v-if="job" class="preview">
      <h4>发布预览</h4><p>{{ job.repository }} · {{ job.tag }} · {{ stateText(job.state) }}</p>
      <p>账号：{{ job.account }}。预览中的归档已冻结；后续本地编辑不会改变本次发布。</p>
      <ul><li v-for="p in job.catalog.packages" :key="p.id">{{ p.name }} · {{ p.id }}@{{ p.version }} · {{ formatSize(p.size) }}<details><summary>SHA256</summary><code>{{ p.sha256 }}</code></details></li></ul>
      <div class="actions">
        <button v-if="job.state !== 'published'" class="btn" :disabled="!!busy" @click="makeDraft">{{ job.state === 'draft' ? '重新校验草稿' : '创建 / 重试草稿' }}</button>
        <button v-if="job.state === 'draft'" class="btn btn-primary" :disabled="!!busy" @click="publish">公开发布</button>
        <a v-if="job.state !== 'prepared'" :href="releaseUrl(job)" target="_blank" rel="noopener noreferrer">查看 Release</a>
      </div>
      <p v-if="job.state === 'published'">已公开。用户可在“配置包”页添加 https://github.com/{{ job.repository }}。</p>
    </div>
    <details><summary>恢复发布任务</summary><button class="btn btn-sm" :disabled="!!busy" @click="loadJobs">刷新任务</button><div v-for="item in jobs" :key="item.id" class="actions"><span>{{ item.repository }} · {{ item.tag }} · {{ stateText(item.state) }}</span><button class="btn btn-sm" :disabled="!!busy" @click="job = item">打开</button></div></details>
  </section>
</template>
<script setup>
import { onMounted, ref } from 'vue'
import { api } from '../../../web/src/api'
import { useConfirmDialog } from '../../../web/src/components/ui/useConfirmDialog'
const confirm = useConfirmDialog()
const repository = ref(''), selected = ref([]), includeMedia = ref(false), notes = ref('')
const packages = ref([]), account = ref(''), busy = ref(''), error = ref(''), message = ref(''), job = ref(null), jobs = ref([])
const call = (action, values = {}) => api.callExtension('gamer-package-publisher', `publisher.${action}`, values)
const stateText = state => ({ prepared: '待创建草稿', draft: '草稿已校验', published: '已公开' }[state] || state)
const formatSize = n => `${(n / 1024).toFixed(1)} KiB`
const releaseUrl = item => item.release_url || `https://github.com/${item.repository}/releases/tag/${encodeURIComponent(item.tag)}`
async function run(label, operation) {
  if (busy.value) return
  busy.value = label; error.value = ''; message.value = ''
  try { await operation() } catch (e) { error.value = e.message || '操作失败' }
  finally { busy.value = ''; await loadJobs() }
}
async function loadJobs() { try { jobs.value = await call('jobs') } catch (e) { if (!error.value) error.value = e.message } }
const checkLogin = () => run('检查登录', async () => { account.value = (await call('status')).account })
const prepare = () => run('准备归档', async () => { job.value = await call('prepare', { repository: repository.value, package_ids: [...selected.value], notes: notes.value, include_media: includeMedia.value }); account.value = job.value.account })
const makeDraft = () => run('上传草稿', async () => { job.value = await call('draft', { job_id: job.value.id }) })
async function publish() {
  if (!job.value || busy.value) return
  if (!await confirm(`以账号 ${job.value.account} 向 ${job.value.repository} 公开这 ${job.value.catalog.packages.length} 个配置包？`, { title: '公开配置包', confirmText: '公开发布' })) return
  await run('公开发布', async () => { job.value = await call('publish', { job_id: job.value.id }); message.value = '配置目录已公开' })
}
async function cancel() { try { await call('cancel'); message.value = '已请求取消，远端草稿保留，可恢复任务重试。' } catch (e) { error.value = e.message } }
onMounted(async () => { try { packages.value = (await api.listPackages()).packages || []; await loadJobs() } catch (e) { error.value = e.message } })
</script>
<style scoped>
.publisher{display:flex;flex-direction:column;gap:14px;padding:16px;overflow:auto}.publisher p{color:var(--text-2);line-height:1.6;margin:0}.publisher label{display:flex;flex-direction:column;gap:6px}.publisher .check,.actions{display:flex;flex-direction:row;align-items:center;gap:8px;flex-wrap:wrap}.publisher fieldset,.preview{border:1px solid var(--border);padding:14px;display:flex;flex-direction:column;gap:10px}.publisher .error{color:var(--danger)}.publisher code{overflow-wrap:anywhere}.publisher li{margin:8px 0}.publisher a{overflow-wrap:anywhere}
</style>
