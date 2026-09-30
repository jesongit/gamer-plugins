<template>
  <section class="automation-workbench">
    <nav class="workbench-tabs" aria-label="自动化子页签">
      <button v-for="tab in tabs" :key="tab.key" type="button" class="tab-btn"
        :class="{ active: activeTab === tab.key }" :aria-pressed="activeTab === tab.key"
        :disabled="switching" @click="selectTab(tab.key)">{{ tab.label }}</button>
    </nav>
    <KeepAlive>
      <ScriptRunner v-if="activeTab !== 'templates'" ref="editor" :key="activeTab" :context="activeTab === 'functions' ? ctx.functions : ctx.scripts" />
    </KeepAlive>
    <TemplateCapture v-if="activeTab === 'templates'" :context="ctx.templates" />
  </section>
</template>

<script setup>
import { computed, reactive, ref } from 'vue'
import ScriptRunner from './ScriptRunner.vue'
import TemplateCapture from './TemplateCapture.vue'

const props = defineProps({ context: { type: Object, required: true } })
const ctx = reactive(props.context)
const tabs = [{ key: 'scripts', label: '脚本' }, { key: 'functions', label: '函数' }, { key: 'templates', label: '模板' }]
const activeTab = computed(() => ctx.scripts.automationTab)
const editor = ref(null), switching = ref(false)
async function selectTab(key) {
  if (switching.value || key === activeTab.value) return
  const previous = activeTab.value
  switching.value = true
  try {
    if (previous !== 'templates' && !await editor.value?.beforeTabChange()) return
    if (activeTab.value === previous) ctx.scripts.automationTab = key
  } finally { switching.value = false }
}
</script>

<style scoped>
.automation-workbench{display:flex;flex-direction:column;flex:1;min-height:0;gap:8px}
.workbench-tabs{display:flex;gap:4px;flex-shrink:0;border-bottom:1px solid var(--border);padding-bottom:6px}
.tab-btn{height:28px;padding:3px 10px;border:1px solid transparent;border-radius:3px;background:transparent;color:var(--text-2);font-size:13px;cursor:pointer}
.tab-btn:hover{color:var(--text-0)}
.tab-btn.active{border-color:var(--border);background:var(--bg-2);color:var(--text-0);font-weight:700}
.tab-btn:disabled{opacity:.5;cursor:default}
</style>
