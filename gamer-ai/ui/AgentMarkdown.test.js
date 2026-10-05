import { expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import AgentMarkdown from './AgentMarkdown.vue'

it('流式 Markdown 保留中文列表、表格和代码，原始 HTML 不执行', async () => {
  const wrapper=mount(AgentMarkdown,{props:{text:'# 攻略\n\n先**检查背包**。\n\n- 避开火焰\n- 准备药剂\n\n<script>alert(1)</script>'}})
  try {
    expect(wrapper.findAll('li')).toHaveLength(2)
    expect(wrapper.get('strong').text()).toBe('检查背包')
    expect(wrapper.find('script').exists()).toBe(false)
    await wrapper.setProps({text:'|步骤|操作|\n|---|---|\n|1|检查背包|\n\n```js\nconst ready = true\n```'})
    expect(wrapper.get('table td').text()).toBe('1')
    expect(wrapper.get('pre code').text()).toContain('const ready')
  } finally {wrapper.unmount()}
})

it('链接拒绝执行协议，安全外链隔离，图片不自动请求外部地址', () => {
  const wrapper=mount(AgentMarkdown,{props:{text:'[资料](https://example.com/guide) [执行](javascript:alert(1)) ![追踪图](https://example.com/pixel.png) <img src=x onerror=alert(1)>'}})
  try {
    expect(wrapper.findAll('a')).toHaveLength(1)
    expect(wrapper.get('a').attributes()).toMatchObject({href:'https://example.com/guide',target:'_blank',rel:'noopener noreferrer'})
    expect(wrapper.find('img').exists()).toBe(false)
    expect(wrapper.text()).toContain('追踪图')
  } finally {wrapper.unmount()}
})
