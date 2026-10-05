<script setup>
import { computed } from 'vue'
import MarkdownIt from 'markdown-it'

const props = defineProps({ text: { type: String, default: '' } })
const markdown = new MarkdownIt({ html: false, breaks: true, linkify: true })
// Model output is untrusted. Navigation stays HTTP(S); screenshots use receipts.
markdown.validateLink = value => /^https?:\/\//i.test(value)
const originalLink = markdown.renderer.rules.link_open || ((tokens, index, options, env, self) => self.renderToken(tokens, index, options))
markdown.renderer.rules.link_open = (tokens, index, options, env, self) => {
  tokens[index].attrSet('target', '_blank')
  tokens[index].attrSet('rel', 'noopener noreferrer')
  return originalLink(tokens, index, options, env, self)
}
markdown.renderer.rules.image = (tokens, index) => markdown.utils.escapeHtml(tokens[index].content || '图片')
const rendered = computed(() => markdown.render(props.text))
</script>

<template><div class="agent-markdown" v-html="rendered" /></template>

<style scoped>
.agent-markdown{min-width:0;overflow-wrap:anywhere;font-size:13px;line-height:1.8}
.agent-markdown :deep(p){margin:0 0 10px;white-space:normal}.agent-markdown :deep(p:last-child){margin-bottom:0}
.agent-markdown :deep(h1),.agent-markdown :deep(h2),.agent-markdown :deep(h3),.agent-markdown :deep(h4){font-size:1em;line-height:1.7;margin:16px 0 7px;font-weight:650}
.agent-markdown :deep(h1:first-child),.agent-markdown :deep(h2:first-child),.agent-markdown :deep(h3:first-child){margin-top:0}
.agent-markdown :deep(ul),.agent-markdown :deep(ol){padding-left:22px;margin:6px 0 12px}.agent-markdown :deep(li+li){margin-top:3px}
.agent-markdown :deep(a){color:var(--accent,#e4c956);text-decoration:none;border-bottom:1px dotted currentColor}.agent-markdown :deep(a:hover){text-decoration:underline}
.agent-markdown :deep(code){font:12px/1.7 Consolas,monospace;background:var(--bg-2,#24282a);padding:2px 4px;border-radius:3px}
.agent-markdown :deep(pre){white-space:pre;overflow:auto;max-width:100%;background:var(--bg-2,#24282a);padding:12px;border-radius:7px;margin:10px 0}
.agent-markdown :deep(pre code){padding:0;background:transparent}.agent-markdown :deep(blockquote){margin:9px 0;padding-left:12px;border-left:2px solid var(--border,#41484a);color:var(--text-2,#aab5b2)}
.agent-markdown :deep(table){border-collapse:collapse;display:block;max-width:100%;overflow:auto;margin:10px 0}.agent-markdown :deep(th),.agent-markdown :deep(td){padding:6px 9px;border:1px solid var(--border,#41484a);text-align:left}
</style>
