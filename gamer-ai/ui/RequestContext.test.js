import { expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import RequestContext from './RequestContext.vue'
import { publicRequest, requestItems, requestTools, toolAccess } from './request-context'

const snapshot = {
  protocol:'responses',model:'glm-test',endpoint:'https://api.example.test/responses',
  request_body:{ model:'glm-test',reasoning:{ effort:'high',summary:'auto' },max_output_tokens:16384,input:[
    {role:'system',content:'基础系统提示词\n第二行完整内容'},
    {role:'developer',content:'运行时只读权限'},
    {role:'user',content:'当前用户的问题'},
    {role:'user',content:[{type:'input_text',text:'用户之后注入的记忆'},{type:'input_image',image_url:'data:image/png;base64,DO_NOT_SHOW_BYTES'}]},
    {type:'function_call_output',call_id:'c1',output:'实际工具结果'},
  ],tools:[{type:'function',name:'memory_search',description:'搜索记忆',parameters:{type:'object',properties:{query:{type:'string'}},required:['query']}}] },
  redactions:['图片字节已省略'],
}
const entry = (seq = 3,scope = 'chat',body = snapshot) => ({id:`prompt:${seq}`,seq,at:'2026-10-04T07:00:00+08:00',data:{scope,prompt_version:'v1',turn_id:'turn',snapshot:body}})
it('真实消息保持完整顺序和公开输出配置，不把用户之后的记忆伪装为前置system',()=>{
  const items=requestItems(snapshot)
  expect(items.map(item=>item.role)).toEqual(['system','developer','user','user','function_call_output'])
  expect(items[0].text).toBe('基础系统提示词\n第二行完整内容')
  expect(items[3].text).toContain('用户之后注入的记忆')
  expect(publicRequest(snapshot.request_body).reasoning).toEqual({effort:'high',summary:'auto'})
  expect(requestTools(snapshot)[0].name).toBe('memory_search')
})
it('协议instructions及Chat messages也按实际输入结构展示',()=>{
  expect(requestItems({request_body:{instructions:'顶层系统指令',input:[{role:'user',content:'输入'}]}}).map(item=>item.text)).toEqual(['顶层系统指令','输入'])
  expect(requestItems({request_body:{messages:[{role:'system',content:'Chat系统'},{role:'user',content:'Chat问题'}],tools:[{type:'function',function:{name:'input_tap'}}]}}).map(item=>item.role)).toEqual(['system','user'])
  expect(toolAccess({request_body:{tools:[{type:'function',function:{name:'input_tap'}}]}},'game')).toContain('已提供设备')
})
it('最新快照默认展开系统，早期请求可追查，不执行内容或显示凭据图片字节',()=>{
  const malicious=structuredClone(snapshot)
  malicious.request_body.input[0].content='<script>alert(1)</script><img src="https://invalid.example/x">'
  malicious.request_body.authorization='Bearer private-credential'
  malicious.request_body.encrypted_content='private-reasoning-data'
  const wrapper=mount(RequestContext,{props:{contexts:[entry(2),entry(3,'chat',malicious)],expanded:true}})
  expect(wrapper.get('.request-context').attributes('open')).toBeDefined()
  expect(wrapper.findAll('.request-messages li')[0].find('details').attributes('open')).toBeDefined()
  expect(wrapper.find('script').exists()).toBe(false)
  expect(wrapper.find('img').exists()).toBe(false)
  expect(wrapper.text()).toContain('<script>alert(1)</script>')
  expect(wrapper.text()).not.toContain('DO_NOT_SHOW_BYTES')
  expect(wrapper.text()).not.toContain('private-credential')
  expect(wrapper.text()).not.toContain('private-reasoning-data')
  expect(wrapper.text()).toContain('本轮仅提供')
  expect(wrapper.text()).toContain('此前 1 轮请求')
  expect(wrapper.text()).toContain('reasoning')
  wrapper.unmount()
})
it('实际编排工具说明统一Agent决策，不要求用户手动切换模式',()=>{
  const value={request_body:{tools:[{type:'function',name:'gameplay_start'},{type:'function',name:'agent_continue'}]}}
  expect(toolAccess(value,'chat')).toContain('Agent 游玩编排工具')
  expect(toolAccess(value,'chat')).toContain('查询或修改记忆不会恢复暂停')
  expect(toolAccess(value,'chat')).not.toContain('选择游玩')
})
it('编辑按钮只请求打开设置，不改变快照或发起设备动作',async()=>{
  const wrapper=mount(RequestContext,{props:{contexts:[entry()],expanded:true}})
  await wrapper.get('button').trigger('click')
  expect(wrapper.emitted('edit')).toHaveLength(1)
  expect(wrapper.props('contexts')[0].data.prompt_version).toBe('v1')
  wrapper.unmount()
})
it('保留工具参数的password/token schema对象，仅脱敏实际凭据标量和数组',()=>{
  const value=publicRequest({tools:[{type:'function',name:'example',parameters:{type:'object',properties:{password:{type:'string',description:'参数说明'},token:{type:'string'},cookie:{type:'boolean'}},required:['password']}}],password:'real-private',token:['real-private'],max_tokens:0})
  expect(value.tools[0].parameters.properties).toEqual({password:{type:'string',description:'参数说明'},token:{type:'string'},cookie:{type:'boolean'}})
  expect(value.tools[0].parameters.required).toEqual(['password'])
  expect(value.password).toBe('[已脱敏]');expect(value.token).toBe('[已脱敏]');expect(value.max_tokens).toBe(0)
  expect(requestItems({request_body:{input:'字符串用户输入'}})[0].text).toBe('字符串用户输入')
})
