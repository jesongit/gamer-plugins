<template>
  <div v-if="!ctx.packageId" class="pkg-empty">
    暂无配置包：请先在投屏上方选择或新建配置包
  </div>
  <template v-else>
    <div class="script-tpl">
      <div class="tpl-top">
        <label class="tpl-setting">相似度<input v-model.number="ctx.testThreshold" class="input input-sm mono" type="number" min="0" max="1" step="0.01" aria-label="匹配阈值" title="0 到 1，越高要求越相似" /></label>
        <label class="tpl-setting tpl-region-setting">识别范围<select v-model="ctx.testRegion" class="select tpl-region" aria-label="匹配区域">
          <option value="">模板默认范围</option>
          <option value="a">全屏</option>
          <option value="u">上半屏</option>
          <option value="d">下半屏</option>
          <option value="l">左半屏</option>
          <option value="r">右半屏</option>
          <option value="ul">左上</option>
          <option value="ur">右上</option>
          <option value="dl">左下</option>
          <option value="dr">右下</option>
        </select></label>
        <input v-model="ctx.tplSearch" class="input input-sm tpl-search" aria-label="搜索模板" placeholder="名称或拼音首字母…" />
        <button class="btn btn-sm" :class="{ active: ctx.picking }" :disabled="!ctx.stageReady" title="在当前画面上截取模板" @click="ctx.togglePick"><UiIcon name="crop" /> {{ ctx.picking ? '取消框选' : '截取模板' }}</button>
        <button class="btn btn-sm" @click="tplUpload.click()" title="选择图片或多选；zip 压缩包自动解压导入（已存在同名跳过）"><UiIcon name="upload" /> 上传</button>
        <input ref="tplUpload" type="file" accept="image/png,image/jpeg,image/webp,image/bmp,image/gif,.zip" multiple hidden @change="ctx.onTplUpload" />
      </div>
      <p v-if="!ctx.stageReady" class="tpl-guidance">连接游戏画面或在视频工作台选择视频后，可截取模板和测试识别；导入图片可离线进行。</p>
      <details class="tpl-help"><summary>相似度与识别范围怎么选</summary><p>相似度阈值为 0 到 1，越高要求图像越相似。默认范围沿用模板记录的区域；更换范围只影响本次测试。测试只检查画面，不执行点击。</p></details>
      <p v-if="!validThreshold" class="tpl-error" role="status">请输入 0 到 1 之间的相似度阈值。</p>
      <div v-if="ctx.matchTest" class="tpl-result" :class="ctx.matchTest.state" role="status" aria-live="polite">
        <strong>{{ { checking: '正在识别…', hit: '找到模板', miss: '当前画面未找到模板', error: '识别未完成' }[ctx.matchTest.state] }}</strong>
        <span>{{ ctx.tplShortName(ctx.matchTest.name) }}<template v-if="ctx.matchTest.score != null"> · 相似度 {{ ctx.matchTest.score.toFixed(2) }}</template></span>
        <small v-if="ctx.matchTest.message">{{ ctx.matchTest.message }}</small><small v-else>{{ ctx.matchTest.source }} · 本次阈值 {{ ctx.matchTest.threshold }}{{ ctx.matchTest.state === 'miss' ? '；请核对画面、模板和识别范围。' : ctx.matchTest.state === 'hit' ? '；匹配框显示在画面上。' : '' }}</small>
      </div>

      <div class="tpl-list-wrap">
        <div class="tpl-list-head">
          <span class="tpl-cell thumb">缩略图</span>
          <span class="tpl-cell name">文件名</span>
          <span class="tpl-cell ops">操作</span>
        </div>
        <div class="tpl-list">
          <div
            v-for="t in ctx.templates"
            :key="t.name"
            class="tpl-row"
            :class="{ renaming: ctx.renaming === t.name }"
            @click="ctx.onTplRowClick($event, t)"
          >
            <span class="tpl-cell thumb" @click.stop="ctx.onTplThumbClick($event, t)">
              <button type="button" class="tpl-thumb" title="查看大图" @click.stop="ctx.onTplThumbClick($event, t)">
                <img :src="ctx.tplThumbUrl(t.name)" alt="" loading="lazy" @error="e => e.target.style.visibility = 'hidden'" />
              </button>
            </span>
            <span class="tpl-cell name mono" :title="ctx.renaming === t.name ? '' : '点击复制短名'" @click.stop="ctx.onTplNameClick($event, t)">
              <input
                v-if="ctx.renaming === t.name"
                :ref="el => ctx.setRenameInputEl(el)"
                v-model="ctx.renameVal"
                class="input rename-input mono"
                @keydown.enter="ctx.confirmRename(t)"
                @keydown.esc="ctx.cancelRename"
                @blur="ctx.cancelRename"
                @click.stop
              />
              <template v-else>
                {{ ctx.tplShortName(t.name) }}<span v-if="ctx.tplRegionBadge(t.name)" class="tpl-region-badge">{{ ctx.tplRegionBadge(t.name) }}</span>
              </template>
            </span>
            <span class="tpl-cell ops">
              <button class="btn btn-sm" :disabled="!ctx.stageReady || !validThreshold || ctx.matchTest?.state === 'checking'" @click.stop="ctx.onTplMatchClick(t)">{{ ctx.matchTest?.state === 'checking' && ctx.matchTest.name === t.name ? '识别中…' : '测试识别' }}</button>
              <span class="tpl-more-wrap" @keydown.esc.stop.prevent="dismissMore">
                <button class="btn btn-sm" :class="{ active: moreOpenName === t.name }" aria-haspopup="menu" :aria-expanded="moreOpenName === t.name" @click.stop="toggleMore(t.name, $event)">更多<span class="menu-chevron" aria-hidden="true"></span></button>
                <Teleport to="body">
                <span v-if="moreOpenName === t.name" class="tpl-more-mask" @click.stop="closeMore"></span>
                <span v-if="moreOpenName === t.name" ref="moreMenuEls" class="tpl-more-dropdown action-menu" :style="moreMenuStyle" role="menu" @keydown.esc.stop.prevent="dismissMore">
                  <button class="tpl-more-item action-menu-item" role="menuitem" @click.stop="openRename(t)">重命名</button>
                  <button class="tpl-more-item action-menu-item" role="menuitem" :disabled="!ctx.stageReady || ctx.saving" @click.stop="replaceTemplate(t)">替换</button>
                  <span class="action-menu-separator" role="separator"></span>
                  <button class="tpl-more-item action-menu-item danger" role="menuitem" @click.stop="deleteTemplate(t)">删除</button>
                </span>
                </Teleport>
              </span>
            </span>
          </div>
          <div v-if="!ctx.templates.length" class="tpl-empty"><template v-if="ctx.tplSearch.trim()">没有符合搜索条件的模板。<button class="btn btn-sm" @click="ctx.tplSearch = ''">清除搜索</button></template><template v-else>还没有识别模板。可从当前画面截取按钮或完成标记，也可以在上方导入图片或压缩包。</template></div>
        </div>
      </div>
    </div>

    <!-- 二次裁切弹窗已拆到 TemplateCropModal.vue（挂在面板层级，任何页签下框选都可见） -->
    <div v-if="ctx.viewTpl" class="tpl-view-mask" v-backdrop-dismiss="ctx.closeTplView">
      <div class="tpl-view-modal">
        <button class="tpl-view-close" aria-label="关闭模板预览" @click="ctx.closeTplView"><UiIcon name="close" /></button>
        <div class="tpl-view-img"><img :src="ctx.tplThumbUrl(ctx.viewTpl)" alt="模板预览" /></div>
        <div class="tpl-view-name mono">{{ ctx.viewTpl }}</div>
      </div>
    </div>
  </template>
</template>

<script setup>
import { vBackdropDismiss } from '../../../../../ui-shared/backdrop-dismiss.js'
import UiIcon from '../../../../../../web/src/components/ui/UiIcon.vue'
import { computed, nextTick, onMounted, onUnmounted, reactive, ref } from 'vue'

const props = defineProps({ context: { type: Object, required: true } })
const ctx = reactive(props.context)
const validThreshold = computed(() => ctx.testThreshold !== '' && ctx.testThreshold != null && Number.isFinite(Number(ctx.testThreshold)) && Number(ctx.testThreshold) >= 0 && Number(ctx.testThreshold) <= 1)
const tplUpload = ref(null)
const moreOpenName = ref(null)
const moreMenuEls = ref([])
const moreMenuStyle = ref({})
let moreTrigger = null

function closeMore() {
  moreOpenName.value = null
}

function dismissMore() {
  closeMore()
  moreTrigger?.focus()
}

async function toggleMore(name, event) {
  if (moreOpenName.value === name) return closeMore()
  moreTrigger = event.currentTarget
  const anchor = moreTrigger.getBoundingClientRect()
  moreMenuStyle.value = { left: `${Math.max(8, Math.min(anchor.right - 140, window.innerWidth - 148))}px`, top: `${anchor.bottom + 4}px` }
  moreOpenName.value = name
  await nextTick()
  if (moreOpenName.value !== name) return
  const menu = moreMenuEls.value[0]
  if (!menu) return
  const height = menu.getBoundingClientRect().height
  const top = anchor.bottom + 4 + height <= window.innerHeight - 8 ? anchor.bottom + 4 : Math.max(8, anchor.top - height - 4)
  moreMenuStyle.value.top = `${top}px`
  menu.querySelector('button')?.focus({ preventScroll: true })
}

function onMenuScroll(event) {
  if (!event.target?.closest?.('.tpl-more-dropdown')) closeMore()
}
onMounted(() => { window.addEventListener('resize', closeMore); window.addEventListener('scroll', onMenuScroll, true) })
onUnmounted(() => { window.removeEventListener('resize', closeMore); window.removeEventListener('scroll', onMenuScroll, true) })

function openRename(t) {
  closeMore()
  ctx.startRename(t)
}

async function deleteTemplate(t) {
  await ctx.onTplDeleteClick(t)
  closeMore()
}

function replaceTemplate(t) {
  closeMore()
  ctx.replaceTemplate(t)
}
</script>

<style scoped>
.pkg-empty{flex:none;padding:24px 10px;text-align:center;font-size:12px;color:var(--text-2)}
.script-tpl{flex:1;min-height:0;display:flex;flex-direction:column;gap:8px}.tpl-top{display:flex;align-items:center;gap:8px}.tpl-top .input{flex:2 1 0%;min-width:0}.tpl-top .tpl-region{flex:4 1 0%;min-width:0;padding:4px 6px;font-size: 12px}.tpl-top .tpl-search{flex:5 1 0%;min-width:0;font-size: 12px}.tpl-top .btn{flex:3 1 0%;min-width:0}.tpl-list-wrap{flex:1;min-height:0;display:flex;flex-direction:column;gap:4px}.tpl-list-head,.tpl-row{display:flex;align-items:center;gap:8px;padding:3px 8px}.tpl-list-head{font-size: 12px;color:var(--text-2);border-bottom:1px solid var(--border);flex-shrink:0}.tpl-list{flex:1;overflow:auto;display:flex;flex-direction:column;gap:2px;min-height:0}.tpl-row{cursor:pointer;border-radius:var(--radius-sm);border:1px solid transparent}.tpl-row:hover{background:var(--bg-3)}.tpl-row.del-confirm{background:rgba(248,113,113,.08);border-color:rgba(248,113,113,.35)}.tpl-row.renaming{background:color-mix(in srgb, var(--accent) 8%, transparent);border-color:color-mix(in srgb, var(--accent) 35%, transparent)}.tpl-empty{padding:16px 8px;text-align:center;font-size: 12px;color:var(--text-2)}.tpl-cell.thumb{width:40px;flex-shrink:0;display:flex;align-items:center}.tpl-cell.name{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:12px;color:var(--text-0)}.tpl-cell.ops{display:flex;gap:6px;flex-shrink:0}.tpl-thumb{display:inline-flex;align-items:center;justify-content:center;width:28px;height:28px;padding:0;border:1px solid transparent;border-radius:4px;background:transparent;cursor:pointer}.tpl-thumb:hover{border-color:var(--accent)}.tpl-thumb img{width:24px;height:24px;object-fit:contain}.rename-input{width:100%;min-width:0;padding:2px 6px;font-size:12px}.tpl-region-badge{display:inline-block;margin-left:6px;padding:0 5px;border-radius:4px;background:var(--bg-3);border:1px solid var(--border);color:var(--accent);font-size: 12px;line-height:16px}.tpl-more-wrap{position:relative;display:inline-flex}.tpl-more-mask{position:fixed;inset:0;z-index:20}.tpl-more-dropdown{position:fixed;z-index:30;width:140px;max-height:calc(100vh - 16px);overflow:auto}.tpl-view-mask{position:fixed;inset:0;z-index:100;display:flex;align-items:center;justify-content:center;background:rgba(8,10,16,.78)}.tpl-view-modal{position:relative;display:flex;flex-direction:column;gap:8px;max-width:92vw;max-height:92vh}.tpl-view-img{position:relative;align-self:center}.tpl-view-img img{display:block;max-width:92vw;max-height:82vh;object-fit:contain;border-radius:var(--radius-sm);border:1px solid var(--border);background:#000}.tpl-view-close{position:absolute;top:8px;right:8px;width:28px;height:28px;background:var(--bg-2);border:1px solid var(--border);border-radius:50%;color:var(--text-1);cursor:pointer;z-index:1}.tpl-view-name{text-align:center;font-size:12px;color:var(--text-1);word-break:break-all}
.mono{font-family:var(--mono);font-size: 12px;color:var(--text-1)}
.script-tpl{min-width:0;padding:2px;box-sizing:border-box}
.tpl-top{flex-wrap:wrap;gap:6px}
.tpl-top .input{flex:0 0 60px;min-width:60px}
.tpl-top .tpl-region{flex:1 1 92px;min-width:92px}
.tpl-top .tpl-search{flex:2 1 160px;min-width:160px}
.tpl-top .btn{flex:none}
.tpl-setting{display:flex;align-items:center;gap:6px;flex:none;font-size:13px;color:var(--text-1)}.tpl-region-setting{flex:1 1 205px}.tpl-region-setting .tpl-region{min-width:0;flex:1}.tpl-guidance,.tpl-help p{font-size:13px;color:var(--text-2);line-height:1.7;margin:0}.tpl-help{flex:none;font-size:13px}.tpl-help summary{cursor:pointer;padding:4px 0}.tpl-result{display:flex;flex:none;flex-direction:column;gap:4px;padding:8px;border:1px solid var(--border);border-radius:var(--radius-sm);font-size:13px}.tpl-result small{font-size:12px;color:var(--text-2);line-height:1.6}.tpl-result.hit strong{color:var(--ok)}.tpl-result.miss strong{color:var(--warn)}.tpl-result.error strong,.tpl-error{color:var(--danger)}.tpl-error{font-size:13px}.tpl-empty .btn{margin-top:8px}
</style>
