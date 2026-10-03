import { expect, it } from 'vitest'
import { conversationTurns, mergeEvents, safeDiagnostic, lineDiff } from './conversation-format'
const event = (seq,kind,data={},message='') => ({seq,kind,data,message,at:'2026-10-03T12:00:00Z'})
it('增量按seq去重，用户交付更新原卡，最终文本替换公开增量', () => {
  const events = [event(1,'user',{message_id:'u',turn_id:'t',status:'queued'},'打开设置'),event(2,'assistant_delta',{message_id:'a',turn_id:'t',delta:'开始',channel:'text'})]
  const combined = mergeEvents(events,[events[1],event(3,'assistant_delta',{message_id:'a',turn_id:'t',delta:'观察',channel:'text'}),event(4,'user_status',{message_id:'u',turn_id:'t',status:'incorporated'}),event(5,'assistant_final',{message_id:'a',turn_id:'t',text:'已完成最终说明'}),event(6,'state',{turn_id:'t',state:'idle'})])
  const turn = conversationTurns(combined)[0]
  expect(combined).toHaveLength(6)
  expect(turn.users).toHaveLength(1)
  expect(turn.users[0].status).toBe('incorporated')
  expect(turn.answers).toHaveLength(1)
  expect(turn.answers[0]).toMatchObject({text:'已完成最终说明',status:'complete'})
  expect(turn.completed).toBe(true)
})
it('公开思考与工具共用过程组，旧私有推理不成为展示消息，工具结果更新同卡', () => {
  const turn=conversationTurns([
    event(1,'assistant_delta',{turn_id:'t',message_id:'s',channel:'summary',delta:'先查本包经验'}),
    event(2,'reasoning',{turn_id:'t'},'不可公开'),
    event(3,'tool_start',{turn_id:'t',step_id:'s1',call_id:'c',name:'memory_search',args:{query:'背包'}}),
    event(4,'tool_end',{turn_id:'t',step_id:'s1',call_id:'c',name:'memory_search',ok:true,result:{items:[{id:'m',revision:2}]}}),
    event(5,'state',{turn_id:'t',state:'idle'}),
  ])[0]
  expect(turn.process).toHaveLength(2)
  expect(turn.process[0]).toMatchObject({kind:'thinking',text:'先查本包经验',status:'complete'})
  expect(turn.process[1]).toMatchObject({kind:'tool',status:'complete',data:{args:{query:'背包'},result:{items:[{id:'m',revision:2}]}}})
  expect(JSON.stringify(turn)).not.toContain('不可公开')
})
it('MCP标准结果保留业务收据和安全截图以供工具卡渲染', () => {
  const turn=conversationTurns([event(1,'tool_start',{turn_id:'t',step_id:'s',call_id:'c',name:'memory_get',args:{id:'m'}}),event(2,'tool_end',{turn_id:'t',step_id:'s',call_id:'c',name:'memory_get',result:{content:[{type:'text',text:JSON.stringify({memory:{id:'m',revision:3,effective_validation:'pending'}})},{type:'image',mimeType:'image/png',data:'aGVsbG8='}]}})])[0]
  expect(turn.process).toHaveLength(1)
  expect(turn.process[0].receipt.memory).toMatchObject({id:'m',revision:3,effective_validation:'pending'})
  expect(turn.process[0].image).toBe('data:image/png;base64,aGVsbG8=')
})
it('取消保留已展示前缀并标记中断，不伪造最终结果', () => {
  const turn=conversationTurns([event(1,'assistant_delta',{turn_id:'t',message_id:'a',channel:'text',delta:'已观察到'}),event(2,'state',{turn_id:'t',state:'cancelled'},'用户取消')])[0]
  expect(turn.answers[0]).toMatchObject({text:'已观察到',status:'interrupted'})
  expect(turn.notices[0].text).toBe('用户取消')
})
it('queued无turn_id时，纳入收据按message_id关联且保持原提交顺序',()=>{
 const turns=conversationTurns([event(1,'user',{message_id:'first',status:'queued'},'第一条'),event(2,'user',{message_id:'second',status:'queued'},'第二条'),event(3,'user_status',{message_id:'first',turn_id:'processing',status:'incorporated'}),event(4,'assistant_final',{message_id:'answer',turn_id:'processing',text:'第一条答复',summary:['公开摘要']})])
 expect(turns.map(turn=>turn.users[0].text)).toEqual(['第一条','第二条'])
 expect(turns[0].id).toBe('processing')
 expect(turns[0].answers[0].text).toBe('第一条答复')
 expect(turns[0].process[0].text).toBe('公开摘要')
})
it('诊断保留真实token计数而移除密钥、Cookie、Bearer和私有推理', () => {
  const value=safeDiagnostic({metadata:{known_tokens:100,total_tokens:120,max_tokens:0,api_key:'hidden',cookie:'hidden'},events:[{data:{authorization:'hidden',token:'hidden',reasoning:'hidden',usage:{input_tokens:42}}}]})
  expect(value.metadata).toEqual({known_tokens:100,total_tokens:120,max_tokens:0})
  expect(value.events[0].data.usage.input_tokens).toBe(42)
  expect(JSON.stringify(value)).not.toContain('hidden')
})
it('逐行差异显示新增、删除和未变正文', () => {
  expect(lineDiff('前提\n旧步骤','前提\n新步骤')).toEqual([{type:'same',text:'前提'},{type:'removed',text:'旧步骤'},{type:'added',text:'新步骤'}])
})
