import { beforeEach,afterEach,expect,it,vi } from 'vitest'
import { mount,flushPromises } from '@vue/test-utils'
const mocks=vi.hoisted(()=>({call:vi.fn()}))
vi.mock('../../../web/src/api',()=>({api:{callExtension:mocks.call}}))
import AgentConversation from './AgentConversation.vue'
let wrappers,record,events,session,diagnostics,older
const button=(wrapper,label)=>wrapper.findAll('button').find(item=>item.text()===label)
const limits={max_turns:40,max_actions:120,max_seconds:600,max_tokens:100000,max_failures:3}
const ev=(seq,kind,data={},message='')=>({seq,kind,message,at:'2026-10-03T12:00:00Z',data:{turn_id:'turn-1',...data}})
async function create(props={}){const wrapper=mount(AgentConversation,{props:{packageId:'default',...props}});wrappers.push(wrapper);await flushPromises();return wrapper}
beforeEach(()=>{
  wrappers=[];record=null;events=[];older=[];diagnostics=[];session=[]
  mocks.call.mockReset().mockImplementation(async(_,action,values={})=>{
    if(action==='settings.get')return{has_key:true,model:'test'}
    if(action==='services.get')return{search:{enabled:false}}
    if(action==='session.get')return{sessions:session}
    if(action==='conversation.list')return{conversations:record?[record]:[]}
    if(action==='conversation.create'){record={conversation_id:'c1',content_package:values.content_package,state:'idle',title:'测试',limits:values.limits||limits,usage:{turns:0,actions:0,total_tokens:null}};return{conversation:structuredClone(record)}}
    if(action==='conversation.get'){
      const page=values.before_seq!=null?older:events.filter(event=>values.after_seq==null||event.seq>values.after_seq)
      return{conversation:structuredClone(record),events:structuredClone(page),latest_seq:events.at(-1)?.seq||0,oldest_seq:page[0]?.seq||null,has_more_before:values.before_seq==null&&older.length>0}
    }
    if(action==='conversation.message'){
      events.push(ev(events.length+1,'user',{message_id:'m1',status:'queued'},values.message));record.state='running'
      return{message:{id:'m1',status:'queued'},conversation:structuredClone(record)}
    }
    if(action==='conversation.withdraw'){events.push(ev(events.length+1,'user_status',{message_id:values.message_id,status:'withdrawn'}));return{ok:true}}
    if(action==='conversation.cancel'){record.state='cancelled';events.push(ev(events.length+1,'state',{state:'cancelled'},'已取消'));return{ok:true}}
    if(action==='conversation.diagnostics')return{metadata:{known_tokens:100,api_key:'redact-me'},events:diagnostics}
    throw new Error(`Unexpected ${action}`)
  })
})
afterEach(()=>{wrappers.forEach(wrapper=>wrapper.unmount());vi.useRealTimers();vi.restoreAllMocks()})
it('无设备创建持续对话，显式配置包，服务器接收回显不重复',async()=>{
  const wrapper=await create()
  await wrapper.get('[aria-label="Agent 消息"]').setValue('请先整理背包经验')
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.create',{content_package:'default',limits})
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.message',{conversation_id:'c1',message:'请先整理背包经验',attached_memory:[]})
  expect(wrapper.findAll('.user-message')).toHaveLength(1)
  expect(wrapper.text()).toContain('待处理')
  expect(mocks.call.mock.calls.some(call=>call[1]==='session.start'||call[1]==='session.resume')).toBe(false)
  await button(wrapper,'撤回').trigger('click');await flushPromises()
  expect(wrapper.text()).toContain('已撤回')
})
it.each(['external','package_deleted'])('只读历史 %s 禁止新消息，显示建立新对话提示',async state=>{
  record={conversation_id:'c1',content_package:'default',state,limits}
  const wrapper=await create()
  expect(wrapper.get('[aria-label="Agent 消息"]').element.disabled).toBe(true)
  expect(wrapper.get('.composer button[type="submit"]').element.disabled).toBe(true)
  expect(wrapper.text()).toContain('此记录仅供回看')
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  expect(mocks.call.mock.calls.some(call=>call[1]==='conversation.message')).toBe(false)
})
it('增量查询使用after_seq，公开前缀在同消息更新且final权威',async()=>{
  vi.useFakeTimers({toFake:['setTimeout','clearTimeout']})
  record={conversation_id:'c1',content_package:'default',state:'running',limits}
  events=[ev(1,'user',{message_id:'m1',status:'incorporated'},'检查设置'),ev(2,'assistant_delta',{message_id:'a1',channel:'text',delta:'正在'})]
  const wrapper=await create()
  expect(wrapper.findAll('.final-answer')).toHaveLength(1)
  events.push(ev(3,'assistant_delta',{message_id:'a1',channel:'text',delta:'检查'}))
  await vi.advanceTimersByTimeAsync(500);await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.get',{conversation_id:'c1',after_seq:2,limit:80})
  expect(wrapper.get('.final-answer').text()).toContain('正在检查')
  events.push(ev(4,'assistant_final',{message_id:'a1',text:'新的最终答复'}),ev(5,'state',{state:'idle'}));record.state='idle'
  await vi.advanceTimersByTimeAsync(250);await flushPromises()
  expect(wrapper.findAll('.final-answer')).toHaveLength(1)
  expect(wrapper.get('.final-answer').text()).toContain('新的最终答复')
  expect(wrapper.get('.final-answer').text()).not.toContain('正在检查')
})
it('突发增量分页按实际收到的seq推进，不能跳到服务端全局latest_seq',async()=>{
 vi.useFakeTimers({toFake:['setTimeout','clearTimeout']})
 record={conversation_id:'c1',content_package:'default',state:'running',limits}
 events=[ev(1,'user',{message_id:'m1'},'检查'),ev(2,'assistant_delta',{message_id:'a1',channel:'text',delta:'0'})]
 const wrapper=await create(),implementation=mocks.call.getMockImplementation()
 mocks.call.mockImplementation(async(id,action,values)=>{const result=await implementation(id,action,values);if(action==='conversation.get'&&values.after_seq!=null)result.events=result.events.slice(0,2);return result})
 for(let seq=3;seq<=6;seq++)events.push(ev(seq,'assistant_delta',{message_id:'a1',channel:'text',delta:String(seq-2)}))
 await vi.advanceTimersByTimeAsync(500);await flushPromises()
 await vi.advanceTimersByTimeAsync(250);await flushPromises()
 expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.get',{conversation_id:'c1',after_seq:4,limit:80})
 expect(wrapper.get('.final-answer').text()).toContain('01234')
})
it('历史使用before_seq分页并保留离开底部的阅读位置',async()=>{
  record={conversation_id:'c1',content_package:'default',state:'idle',limits}
  events=[ev(100,'user',{message_id:'u100'},'最新')];older=[ev(1,'user',{message_id:'u1',turn_id:'old'},'超过截断范围的旧记录')]
  const wrapper=await create(),el=wrapper.get('.chat-scroll').element
  let height=400;Object.defineProperty(el,'scrollHeight',{configurable:true,get:()=>height});Object.defineProperty(el,'clientHeight',{configurable:true,value:100});el.scrollTop=120
  await wrapper.get('.chat-scroll').trigger('scroll')
  const implementation=mocks.call.getMockImplementation()
  mocks.call.mockImplementation(async(id,action,values)=>{const result=await implementation(id,action,values);if(action==='conversation.get'&&values.before_seq)height=800;return result})
  await button(wrapper,'加载更早记录').trigger('click');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.get',{conversation_id:'c1',before_seq:100,limit:80})
  expect(wrapper.text()).toContain('超过截断范围的旧记录')
  expect(el.scrollTop).toBe(520)
})
it('取消问答只发conversation.cancel，记忆引用固定revision且游戏恢复需要勾选',async()=>{
  record={conversation_id:'c1',content_package:'default',state:'running',limits}
  session=[{session_id:'s1',device_id:'phone',content_package:'default',mode:'api',state:'paused'}]
  const wrapper=await create({attachedMemory:[{id:'m',revision:3,title:'攻略',content_package:'default'},{id:'foreign',revision:1,content_package:'other'}]})
  await button(wrapper,'取消本轮问答').trigger('click');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.cancel',{conversation_id:'c1'})
  expect(mocks.call.mock.calls.some(call=>call[1]==='session.stop'||call[1]==='session.pause')).toBe(false)
  await wrapper.get('[aria-label="本条消息关联游戏会话"]').setValue('s1')
  await wrapper.get('[aria-label="Agent 消息"]').setValue('请按攻略继续')
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.message',{conversation_id:'c1',message:'请按攻略继续',attached_memory:[{id:'m',revision:3}],game_session_id:'s1',resume:false})
})
it('聊天记忆工具结果展示有效验证状态与旧来源复核警告',async()=>{
  record={conversation_id:'c1',content_package:'default',state:'idle',limits}
  const memory={id:'m1',title:'来源改变的经验',revision:3,validation:'verified',effective_validation:'pending'}
  const conflict={source_id:'source1',cited_revision:1,current_revision:2,deleted:false}
  events=[ev(1,'tool_start',{name:'memory_get',call_id:'c1',step_id:'s1'}),ev(2,'tool_end',{name:'memory_get',call_id:'c1',step_id:'s1',result:{content:[{type:'text',text:JSON.stringify({memory,source_conflicts:[conflict]})}]}}),ev(3,'state',{state:'idle'})]
  const wrapper=await create()
  expect(wrapper.get('.process').text()).toContain('待验证')
  expect(wrapper.get('.process').text()).toContain('资料需复核')
  expect(wrapper.get('.process').text()).toContain('引用修订 r1 → 当前 r2')
  expect(wrapper.get('.process').text()).toContain('保存的验证标记：来源已验证')
})
it('聊天全部预算0会提交，非零秒预算9阻止发送',async()=>{
  const wrapper=await create()
  await button(wrapper,'聊天预算').trigger('click')
  for(const field of ['模型轮数','工具次数','活动秒数','累计 Token','连续失败'])await wrapper.get(`[aria-label="聊天${field}预算"]`).setValue(0)
  await wrapper.get('[aria-label="Agent 消息"]').setValue('讨论方案')
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.create',{content_package:'default',limits:{max_turns:0,max_actions:0,max_seconds:0,max_tokens:0,max_failures:0}})
  await wrapper.get('[aria-label="聊天活动秒数预算"]').setValue(9)
  await wrapper.get('[aria-label="Agent 消息"]').setValue('不应提交')
  expect(wrapper.get('.composer button[type="submit"]').element.disabled).toBe(true)
})
it('游戏持久对话显示独立输入屏障控制，暂停中不误报可人工操作',async()=>{
 record={conversation_id:'s1',content_package:'default',game_session_id:'s1',state:'idle',limits}
 session=[{session_id:'s1',device_id:'phone',content_package:'default',mode:'api',state:'running'}]
 const implementation=mocks.call.getMockImplementation()
 mocks.call.mockImplementation(async(id,action,values)=>{if(action==='session.pause'){session[0].state='pausing';return{}};if(action==='session.stop'){session[0].state='finished';return{}};return implementation(id,action,values)})
 const wrapper=await create()
 expect(wrapper.get('.game-controls').text()).toContain('AI 持有设备控制')
 await button(wrapper,'暂停 AI 游玩').trigger('click');await flushPromises()
 expect(mocks.call).toHaveBeenCalledWith('gamer-ai','session.pause',{session_id:'s1'})
 expect(wrapper.get('.game-controls').text()).toContain('人工仍锁定')
 expect(wrapper.get('.game-controls').text()).not.toContain('游玩暂停，可人工操作')
 await button(wrapper,'停止设备会话').trigger('click');await flushPromises()
 expect(mocks.call).toHaveBeenCalledWith('gamer-ai','session.stop',{session_id:'s1'})
 expect(mocks.call.mock.calls.some(call=>call[1]==='conversation.cancel')).toBe(false)
})
it('诊断分类、定位与导出只读脱敏快照，保留预算计数',async()=>{
  record={conversation_id:'c1',content_package:'default',state:'idle',limits}
  diagnostics=[ev(1,'diagnostic',{category:'model',api_key:'redact-me',usage:{total_tokens:42}},'模型请求'),ev(2,'tool_end',{category:'tool'},'工具回执')]
  const wrapper=await create()
  await button(wrapper,'诊断').trigger('click');await flushPromises()
  await wrapper.get('[aria-label="诊断类别"]').setValue('tool')
  expect(wrapper.get('.diagnostic-drawer').text()).toContain('工具回执')
  expect(wrapper.get('.diagnostic-drawer ol').text()).not.toContain('模型请求')
  const captured=[];vi.spyOn(URL,'createObjectURL').mockImplementation(blob=>{captured.push(blob);return'blob:fixture'});vi.spyOn(URL,'revokeObjectURL').mockImplementation(()=>{});vi.spyOn(HTMLAnchorElement.prototype,'click').mockImplementation(()=>{})
  await button(wrapper,'导出脱敏诊断').trigger('click');await flushPromises()
  const text=await captured[0].text()
  expect(text).toContain('known_tokens');expect(text).toContain('42');expect(text).not.toContain('redact-me')
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.diagnostics',{conversation_id:'c1',export:true})
})
