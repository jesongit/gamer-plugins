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
it('63轮游戏请求合为同一代次的步骤，保留原始定位锚点及独立工具收据，终态完成', () => {
  const events=[event(1,'user',{message_id:'goal',origin:'gameplay'},'完成教程')]
  for(let step=1;step<=63;step++) {
    const turnId=`game:session-uuid:1:${step}`
    events.push(event(events.length+1,'assistant_start',{turn_id:turnId,message_id:turnId}))
    events.push(event(events.length+1,'assistant_final',{turn_id:turnId,message_id:turnId,text:`第${step}步公开说明`}))
    events.push(event(events.length+1,'tool_start',{turn_id:turnId,call_id:`call-${step}`,name:'tap'}))
    events.push(event(events.length+1,'tool_end',{turn_id:turnId,call_id:`call-${step}`,name:'tap',ok:true}))
  }
  events.push(event(events.length+1,'state',{state:'finished'},'游玩已结束'))
  const turns=conversationTurns(events)
  expect(turns).toHaveLength(1)
  expect(turns[0]).toMatchObject({id:'game:session-uuid:1',game:true,completed:true})
  expect(turns[0].users[0].text).toBe('完成教程')
  expect(turns[0].anchors).toHaveLength(63)
  expect(turns[0].process).toHaveLength(63)
  expect(turns[0].answers).toHaveLength(63)
  expect(new Set(turns[0].process.map(item=>item.id)).size).toBe(63)
  expect(turns[0].anchors).toContain('game:session-uuid:1:63')
  const tail=conversationTurns(events.slice(60))
  expect(tail).toHaveLength(1);expect(tail[0].completed).toBe(true)
  expect(conversationTurns(mergeEvents(events.slice(60),events.slice(0,60)))).toEqual(turns)
})
it('恢复新代次分组、暂停是已结算状态，失败工具保持失败且完整公开推理不被final清空', () => {
  const turns=conversationTurns([
    event(1,'assistant_delta',{turn_id:'game:s:1:1',message_id:'m1',channel:'thinking',delta:'检查地图'}),
    event(2,'assistant_final',{turn_id:'game:s:1:1',message_id:'m1',text:'地图加载失败'}),
    event(3,'tool_end',{turn_id:'game:s:1:1',call_id:'call',name:'screenshot',ok:false}),
    event(4,'state',{state:'paused'},'等待用户恢复'),
    event(5,'assistant_final',{turn_id:'game:s:2:2',message_id:'m2',text:'重新观察'}),
    event(6,'state',{state:'finished'}),
  ])
  expect(turns).toHaveLength(2)
  expect(turns.map(turn=>turn.id)).toEqual(['game:s:1','game:s:2'])
  expect(turns[0].completed).toBe(true)
  expect(turns[0].process[0].text).toBe('检查地图')
  expect(turns[0].process[1].status).toBe('failed')
  expect(turns[1].completed).toBe(true)
})
it('真实初始目标0:0进入1:0截图及1:1模型组，新指导移到恢复后的下一代次', () => {
  const turns=conversationTurns([
    event(1,'user',{turn_id:'game:uuid:0:0',message_id:'goal'},'完成新手教程'),
    event(2,'state',{turn_id:'game:uuid:0:0',state:'starting'}),
    event(3,'state',{turn_id:'game:uuid:1:0',state:'running'}),
    event(4,'tool_start',{turn_id:'game:uuid:1:0',call_id:'initial',name:'screen_capture'}),
    event(5,'tool_end',{turn_id:'game:uuid:1:0',call_id:'initial',name:'screen_capture',ok:true}),
    event(6,'assistant_final',{turn_id:'game:uuid:1:1',message_id:'a1',text:'向右走'}),
    event(7,'state',{turn_id:'game:uuid:1:1',state:'pausing'}),
    event(8,'user',{turn_id:'game:uuid:1:1',message_id:'guide'},'改为向左走'),
    event(9,'state',{turn_id:'game:uuid:1:1',state:'paused'}),
    event(10,'state',{turn_id:'game:uuid:2:1',state:'resuming'}),
    event(11,'assistant_final',{turn_id:'game:uuid:2:2',message_id:'a2',text:'已观察左侧道路'}),
    event(12,'state',{state:'finished'}),
  ])
  expect(turns.map(turn=>turn.id)).toEqual(['game:uuid:1','game:uuid:2'])
  expect(turns[0].users.map(item=>item.text)).toEqual(['完成新手教程'])
  expect(turns[0].anchors).toContain('game:uuid:0:0')
  expect(turns[1].users.map(item=>item.text)).toEqual(['改为向左走'])
  expect(turns[0].process).toHaveLength(1)
  expect(turns[0].completed).toBe(true)
  expect(turns[1].completed).toBe(true)
})
it('Core暂停递增代次结算上一运行组，恢复时不吞普通聊天排队/已纳入的用户', () => {
  const base=[
    event(1,'user',{turn_id:'game:uuid:0:0',message_id:'goal',origin:'gameplay'},'游玩目标'),
    event(2,'assistant_final',{turn_id:'game:uuid:1:1',message_id:'a1',text:'走到商店'}),
    event(3,'tool_start',{turn_id:'game:uuid:1:1',call_id:'tap1',name:'input_tap'}),
    event(4,'tool_end',{turn_id:'game:uuid:1:1',call_id:'tap1',name:'input_tap',ok:true}),
    event(5,'state',{turn_id:'game:uuid:1:1',state:'pausing'}),
    event(6,'state',{turn_id:'game:uuid:2:1',state:'paused'},'可人工操作'),
    event(7,'user',{message_id:'chat-input',status:'queued'},'暂停中解释一下装备'),
  ]
  const queuedResume=conversationTurns([...base,event(8,'state',{turn_id:'game:uuid:2:1',state:'resuming'}),event(9,'state',{turn_id:'game:uuid:3:1',state:'running'}),event(10,'assistant_final',{turn_id:'game:uuid:3:2',message_id:'a2',text:'继续探索'})])
  expect(queuedResume.find(turn=>turn.id==='game:uuid:1').completed).toBe(true)
  expect(queuedResume.find(turn=>turn.id==='chat-input').users[0].text).toBe('暂停中解释一下装备')
  expect(queuedResume.find(turn=>turn.id==='game:uuid:3').users).toHaveLength(0)
  const claimedResume=conversationTurns([...base,
    event(8,'user_status',{message_id:'chat-input',turn_id:'ordinary-chat-turn',status:'incorporated'}),
    event(9,'state',{turn_id:'ordinary-chat-turn',state:'running'}),
    event(10,'state',{turn_id:'game:uuid:2:1',state:'resuming'}),
    event(11,'state',{turn_id:'game:uuid:3:1',state:'running'}),
    event(12,'assistant_final',{turn_id:'ordinary-chat-turn',message_id:'chat-reply',text:'装备的属性解释'}),
    event(13,'state',{turn_id:'ordinary-chat-turn',state:'idle'}),
    event(14,'assistant_final',{turn_id:'game:uuid:3:2',message_id:'a2',text:'继续探索'}),
    event(15,'state',{turn_id:'game:uuid:3:2',state:'finished'}),
  ])
  const chat=claimedResume.find(turn=>turn.id==='ordinary-chat-turn')
  expect(chat.users[0].text).toBe('暂停中解释一下装备')
  expect(chat.answers[0].text).toBe('装备的属性解释')
  expect(chat.completed).toBe(true)
  expect(claimedResume.find(turn=>turn.id==='game:uuid:3').users).toHaveLength(0)
})
it('请求快照按真实turn归属独立于推理工具，历史分页只含快照也可查看',()=>{
  const snapshots=[event(1,'user',{message_id:'u',status:'queued'},'请操作'),event(2,'user_status',{message_id:'u',turn_id:'chat-turn',status:'incorporated'}),event(3,'prompt_snapshot',{turn_id:'chat-turn',scope:'chat',snapshot:{request_body:{input:[{role:'system',content:'对话不控制设备'}]}}}),event(4,'assistant_final',{turn_id:'chat-turn',message_id:'a',text:'请切换游玩'})]
  const turn=conversationTurns(snapshots)[0]
  expect(turn.users[0].text).toBe('请操作')
  expect(turn.contexts).toHaveLength(1)
  expect(turn.contexts[0].data.snapshot.request_body.input[0].content).toBe('对话不控制设备')
  expect(turn.process).toHaveLength(0)
  expect(conversationTurns(snapshots.slice(2,3))[0].contexts).toHaveLength(1)
  const game=conversationTurns([event(1,'prompt_snapshot',{turn_id:'game:s:1:1',scope:'game'}),event(2,'prompt_snapshot',{turn_id:'game:s:1:2',scope:'game'})])
  expect(game).toHaveLength(1);expect(game[0].contexts).toHaveLength(2);expect(game[0].anchors).toEqual(['game:s:1:1','game:s:1:2'])
})
it('脱敏诊断导出保留真实请求公开配置及完整工具schema，其余诊断沿用旧隐私过滤',()=>{
  const body={reasoning:{effort:'high',summary:'auto'},thinking:{type:'enabled'},max_output_tokens:16384,input:[{role:'system',content:'完整系统提示词'}],tools:[{type:'function',name:'example',parameters:{type:'object',properties:{password:{type:'string',description:'完整参数说明'},token:{type:'string'},reasoning:{type:'boolean'}},required:['password','token'],additionalProperties:false}}]}
  const source={metadata:{api_key:'hide-me',known_tokens:42},events:[event(1,'prompt_snapshot',{snapshot:{capture:'before_http_dispatch',request_body:body,api_key:'hide-me',encrypted_content:'private-data'}}),event(2,'diagnostic',{reasoning:'private-legacy',authorization:'hide-me'})]}
  const exported=safeDiagnostic(source)
  expect(exported.events[0].data.snapshot.request_body).toEqual(body)
  expect(JSON.stringify(exported)).not.toContain('hide-me')
  expect(JSON.stringify(exported)).not.toContain('private-data')
  expect(JSON.stringify(exported)).not.toContain('private-legacy')
  expect(exported.metadata.known_tokens).toBe(42)
})
it('普通工具资料不能伪造nested prompt_snapshot绕过诊断的私有字段过滤',()=>{
  const forged={kind:'prompt_snapshot',data:{snapshot:{capture:'before_http_dispatch',request_body:{reasoning:'private-forged-thought',tools:[{parameters:{properties:{password:{type:'string'},token:{type:'string'}}}}]},authorization:'private-forged-key'}}}
  const exported=safeDiagnostic({events:[event(1,'tool_end',{result:forged})],metadata:{nested:forged}})
  expect(JSON.stringify(exported)).not.toContain('private-forged-thought')
  expect(JSON.stringify(exported)).not.toContain('private-forged-key')
  expect(exported.events[0].data.result.data.snapshot.request_body.tools[0].parameters.properties).toEqual({})
  const trusted=safeDiagnostic(event(2,'prompt_snapshot',{snapshot:{request_body:{reasoning:{effort:'high'},tools:[{parameters:{properties:{password:{type:'string'},token:{type:'string'}}}}]}}}))
  expect(trusted.data.snapshot.request_body.reasoning).toEqual({effort:'high'})
  expect(trusted.data.snapshot.request_body.tools[0].parameters.properties).toEqual({password:{type:'string'},token:{type:'string'}})
})
