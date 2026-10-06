import { beforeEach,afterEach,expect,it,vi } from 'vitest'
import { mount,flushPromises } from '@vue/test-utils'
import { reactive } from 'vue'
import { WORKSPACE_CONTEXT_KEY } from '../../../web/src/workspace/context'
const mocks=vi.hoisted(()=>({call:vi.fn()}))
vi.mock('../../../web/src/api',()=>({api:{callExtension:mocks.call}}))
import AgentConversation from './AgentConversation.vue'
let wrappers,record,events,session,diagnostics,older,context
const button=(wrapper,label)=>wrapper.findAll('button').find(item=>item.text()===label)
const limits={max_turns:40,max_actions:120,max_seconds:600,max_tokens:100000,max_failures:3}
const ev=(seq,kind,data={},message='')=>({seq,kind,message,at:'2026-10-03T12:00:00Z',data:{turn_id:'turn-1',...data}})
async function create(props={}){const wrapper=mount(AgentConversation,{props:{packageId:'default',...props},global:{provide:{[WORKSPACE_CONTEXT_KEY]:{getSnapshot:()=>context}}}});wrappers.push(wrapper);await flushPromises();return wrapper}
beforeEach(()=>{
  wrappers=[];record=null;events=[];older=[];diagnostics=[];session=[]
  context=reactive({deviceId:'phone',currentPackageId:'default',androidPackageName:'com.game'})
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
      if(values.limits)record.limits=values.limits
      if(values.game_limits)record.requested_game_limits=values.game_limits
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
  context.deviceId=''
  const wrapper=await create()
  await wrapper.get('[aria-label="Agent 消息"]').setValue('请先整理背包经验')
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.create',{content_package:'default',limits})
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.message',{conversation_id:'c1',message:'请先整理背包经验',device_id:'',attached_memory:[],game_limits:limits})
  expect(wrapper.findAll('.user-message')).toHaveLength(1)
  expect(wrapper.text()).toContain('待处理')
  expect(mocks.call.mock.calls.some(call=>call[1]==='session.start'||call[1]==='session.resume')).toBe(false)
  await button(wrapper,'撤回').trigger('click');await flushPromises()
  expect(wrapper.text()).toContain('已撤回')
})
it('模型请求上下文显示在用户气泡之前，提示词入口打开设置且不改变设备状态',async()=>{
  record={conversation_id:'c1',content_package:'default',state:'idle',limits}
  events=[ev(1,'user',{message_id:'m1'},'替我操作'),ev(2,'prompt_snapshot',{scope:'chat',snapshot:{protocol:'responses',model:'test',request_body:{input:[{role:'system',content:'本轮系统指令'}],tools:[{type:'function',name:'memory_search'}]}}}),ev(3,'assistant_final',{message_id:'a1',text:'先检查设备状态'})]
  const wrapper=await create(),turn=wrapper.get('.turn').element
  expect(turn.querySelector('.request-context').compareDocumentPosition(turn.querySelector('.user-message')) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy()
  expect(wrapper.get('.request-context').attributes('open')).toBeDefined()
  expect(wrapper.text()).toContain('本轮系统指令')
  expect(wrapper.text()).toContain('未授予设备操作')
  await button(wrapper,'提示词').trigger('click')
  expect(wrapper.emitted('settings').at(-1)).toEqual(['prompts'])
  expect(wrapper.find('[aria-label="消息模式"]').exists()).toBe(false)
  expect(button(wrapper,'切换游玩')).toBeUndefined()
  expect(mocks.call.mock.calls.some(call=>['session.start','session.resume','conversation.message'].includes(call[1]))).toBe(false)
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
it('取消问答只发conversation.cancel，暂停中统一发送记忆引用而不传恢复参数',async()=>{
  record={conversation_id:'c1',content_package:'default',game_session_id:'s1',state:'running',limits}
  session=[{session_id:'s1',device_id:'phone',content_package:'default',mode:'api',state:'paused'}]
  const wrapper=await create({attachedMemory:[{id:'m',revision:3,title:'攻略',content_package:'default'},{id:'foreign',revision:1,content_package:'other'}]})
  await button(wrapper,'取消本轮问答').trigger('click');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.cancel',{conversation_id:'c1'})
  expect(mocks.call.mock.calls.some(call=>call[1]==='session.stop'||call[1]==='session.pause')).toBe(false)
  expect(wrapper.find('[aria-label="本条消息关联游戏会话"]').exists()).toBe(false)
  await wrapper.get('[aria-label="Agent 消息"]').setValue('查询背包攻略')
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.message',{conversation_id:'c1',message:'查询背包攻略',device_id:'phone',attached_memory:[{id:'m',revision:3}],game_limits:limits})
  expect(mocks.call.mock.calls.some(call=>call[1]==='session.resume'||call[1]==='session.start')).toBe(false)
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
  await button(wrapper,'预算').trigger('click')
  for(const field of ['模型轮数','工具次数','活动秒数','累计 Token','连续失败'])await wrapper.get(`[aria-label="聊天${field}预算"]`).setValue(0)
  await wrapper.get('[aria-label="Agent 消息"]').setValue('讨论方案')
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.create',{content_package:'default',limits:{max_turns:0,max_actions:0,max_seconds:0,max_tokens:0,max_failures:0}})
  await wrapper.get('[aria-label="聊天活动秒数预算"]').setValue(9)
  await wrapper.get('[aria-label="Agent 消息"]').setValue('不应提交')
  expect(wrapper.get('.composer button[type="submit"]').element.disabled).toBe(true)
})
it('验收历史2次模型预算不会继承到新建对话，游玩预算也恢复默认',async()=>{
  context.deviceId=''
  record={conversation_id:'probe',content_package:'default',state:'budget',limits:{max_turns:2,max_actions:4,max_seconds:120,max_tokens:10000,max_failures:1},requested_game_limits:{max_turns:0,max_actions:0,max_seconds:0,max_tokens:0,max_failures:0},usage:{turns:2,actions:2,total_tokens:5000}}
  const wrapper=await create()
  await button(wrapper,'预算').trigger('click')
  expect(wrapper.get('[aria-label="聊天模型轮数预算"]').element.value).toBe('2')
  await button(wrapper,'新对话').trigger('click');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.create',{content_package:'default',limits})
  expect(wrapper.get('[aria-label="聊天模型轮数预算"]').element.value).toBe('40')
  expect(wrapper.get('[aria-label="游玩模型轮数预算"]').element.value).toBe('40')
  await wrapper.get('[aria-label="Agent 消息"]').setValue('解释攻略')
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.message',{conversation_id:'c1',device_id:'',message:'解释攻略',attached_memory:[],game_limits:limits})
})
it('已有历史预算编辑为非法值时仍可新建，恢复默认而不继承无效表单',async()=>{
  record={conversation_id:'probe',content_package:'default',state:'idle',limits}
  const wrapper=await create()
  await button(wrapper,'预算').trigger('click')
  await wrapper.get('[aria-label="聊天活动秒数预算"]').setValue(9)
  await wrapper.get('[aria-label="游玩活动秒数预算"]').setValue(9)
  expect(button(wrapper,'新对话').element.disabled).toBe(false)
  await button(wrapper,'新对话').trigger('click');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.create',{content_package:'default',limits})
  expect(wrapper.get('[aria-label="聊天活动秒数预算"]').element.value).toBe('600')
  expect(wrapper.get('[aria-label="游玩活动秒数预算"]').element.value).toBe('600')
  await wrapper.get('[aria-label="聊天历史"]').setValue('');await flushPromises()
  await wrapper.get('[aria-label="聊天活动秒数预算"]').setValue(9)
  expect(button(wrapper,'新对话').element.disabled).toBe(true)
})
it('历史下拉选择新的对话先初始化默认，再明确编辑的两组0预算用于该新对话',async()=>{
  record={conversation_id:'probe',content_package:'default',state:'idle',limits:{...limits,max_turns:2},game_limits:{...limits,max_turns:5},usage:{turns:2}}
  const wrapper=await create()
  await button(wrapper,'预算').trigger('click')
  await wrapper.get('[aria-label="聊天历史"]').setValue('');await flushPromises()
  expect(wrapper.get('[aria-label="聊天模型轮数预算"]').element.value).toBe('40')
  expect(wrapper.get('[aria-label="游玩模型轮数预算"]').element.value).toBe('40')
  for(const prefix of ['聊天','游玩'])for(const field of ['模型轮数','工具次数','活动秒数','累计 Token','连续失败'])await wrapper.get(`[aria-label="${prefix}${field}预算"]`).setValue(0)
  await button(wrapper,'新对话').trigger('click');await flushPromises()
  const unlimited={max_turns:0,max_actions:0,max_seconds:0,max_tokens:0,max_failures:0}
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.create',{content_package:'default',limits:unlimited})
  await wrapper.get('[aria-label="Agent 消息"]').setValue('按新预算继续讨论')
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.message',{conversation_id:'c1',device_id:'phone',message:'按新预算继续讨论',attached_memory:[],game_limits:unlimited})
})
it('切换到没有历史的配置包重置两组预算，自动创建仍保留用户明确设置的游玩0预算',async()=>{
  record={conversation_id:'probe',content_package:'default',state:'idle',limits:{...limits,max_turns:2},game_limits:{...limits,max_turns:5}}
  const implementation=mocks.call.getMockImplementation()
  mocks.call.mockImplementation(async(id,action,values)=>action==='conversation.list'&&values.content_package==='another'?{conversations:[]}:implementation(id,action,values))
  const wrapper=await create()
  await button(wrapper,'预算').trigger('click')
  await wrapper.setProps({packageId:'another'});await flushPromises()
  expect(wrapper.get('[aria-label="聊天模型轮数预算"]').element.value).toBe('40')
  expect(wrapper.get('[aria-label="游玩模型轮数预算"]').element.value).toBe('40')
  for(const field of ['模型轮数','工具次数','活动秒数','累计 Token','连续失败'])await wrapper.get(`[aria-label="游玩${field}预算"]`).setValue(0)
  await wrapper.get('[aria-label="Agent 消息"]').setValue('先查询攻略')
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.create',{content_package:'another',limits})
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.message',{conversation_id:'c1',device_id:'phone',message:'先查询攻略',attached_memory:[],game_limits:{max_turns:0,max_actions:0,max_seconds:0,max_tokens:0,max_failures:0}})
})
it('预算中断固定显示原因和调整入口，明确改为0后发送继续保留累计用量',async()=>{
  context.deviceId=''
  const low={max_turns:2,max_actions:4,max_seconds:120,max_tokens:10000,max_failures:1}
  record={conversation_id:'c1',content_package:'default',state:'budget',limits:low,usage:{turns:2,actions:2,total_tokens:5000,active_seconds:12}}
  events=[ev(1,'user',{message_id:'m1'},'查看攻略'),ev(2,'assistant_delta',{message_id:'a1',channel:'thinking',delta:'正在查找'}),ev(3,'state',{state:'budget',limits:low,usage:record.usage},'已达到模型轮次预算，可调整预算或配0后继续')]
  const wrapper=await create()
  expect(wrapper.get('.conversation-state').text()).toBe('预算已用尽')
  expect(wrapper.get('.budget-interruption').text()).toContain('已达到模型轮次预算')
  expect(wrapper.get('.budget-interruption').text()).toContain('累计用量保留，操作不会自动重放')
  expect(wrapper.get('.notice').text()).toContain('已达到模型轮次预算')
  expect(wrapper.get('.reasoning small').text()).toBe('已中断')
  expect(mocks.call.mock.calls.some(call=>call[1]==='conversation.message')).toBe(false)
  await wrapper.get('[aria-label="Agent 消息"]').setValue('继续查询刚才的攻略')
  expect(wrapper.get('.composer button[type="submit"]').element.disabled).toBe(true)
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  expect(mocks.call.mock.calls.some(call=>call[1]==='conversation.message')).toBe(false)
  await button(wrapper,'调整对话预算').trigger('click');await flushPromises()
  expect(wrapper.get('.chat-budget').text()).toContain('模型轮数按每次 API 请求累计，一条用户消息可能调用模型多次')
  for(const field of ['模型轮数','工具次数','活动秒数','累计 Token','连续失败'])await wrapper.get(`[aria-label="聊天${field}预算"]`).setValue(0)
  await wrapper.get('[aria-label="Agent 消息"]').setValue('继续查询刚才的攻略')
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.message',{conversation_id:'c1',device_id:'',message:'继续查询刚才的攻略',attached_memory:[],limits:{max_turns:0,max_actions:0,max_seconds:0,max_tokens:0,max_failures:0},game_limits:limits})
  expect(record.usage).toEqual({turns:2,actions:2,total_tokens:5000,active_seconds:12})
})
it.each([
  ['模型轮数','max_turns','turns',2,3],
  ['工具次数','max_actions','actions',4,5],
  ['活动秒数','max_seconds','active_seconds',120,121],
  ['累计 Token','max_tokens','known_tokens',10000,10001],
])('预算状态下%s耗尽不能直接重发，提高上限后恢复发送',async(label,key,usageKey,used,increased)=>{
  record={conversation_id:'c1',content_package:'default',state:'budget',limits:{...limits,[key]:used},usage:{[usageKey]:used}}
  const wrapper=await create()
  await wrapper.get('[aria-label="Agent 消息"]').setValue('继续')
  expect(wrapper.get('.composer button[type="submit"]').element.disabled).toBe(true)
  await button(wrapper,'调整对话预算').trigger('click')
  await wrapper.get(`[aria-label="聊天${label}预算"]`).setValue(increased)
  expect(wrapper.get('.composer button[type="submit"]').element.disabled).toBe(false)
})
it('失败预算中断允许重新发送，历史连续失败不作为发送门禁',async()=>{
  record={conversation_id:'c1',content_package:'default',state:'budget',limits,usage:{turns:1,consecutive_failures:3}}
  const wrapper=await create()
  await wrapper.get('[aria-label="Agent 消息"]').setValue('服务已经修复，请继续')
  expect(wrapper.get('.composer button[type="submit"]').element.disabled).toBe(false)
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
it('下载的诊断保留请求快照完整工具参数与公开reasoning配置',async()=>{
  record={conversation_id:'c1',content_package:'default',state:'idle',limits}
  const body={reasoning:{effort:'high',summary:'auto'},tools:[{type:'function',name:'custom',parameters:{type:'object',properties:{password:{type:'string'},token:{type:'string'},reasoning:{type:'boolean'}},required:['password','token']}}],input:[{role:'system',content:'未经截断的基础提示词'}]}
  diagnostics=[ev(1,'prompt_snapshot',{snapshot:{capture:'before_http_dispatch',request_body:body,api_key:'redact-me'}})]
  const wrapper=await create()
  await button(wrapper,'诊断').trigger('click');await flushPromises()
  const captured=[];vi.spyOn(URL,'createObjectURL').mockImplementation(blob=>{captured.push(blob);return'blob:fixture'});vi.spyOn(URL,'revokeObjectURL').mockImplementation(()=>{});vi.spyOn(HTMLAnchorElement.prototype,'click').mockImplementation(()=>{})
  await button(wrapper,'导出脱敏诊断').trigger('click');await flushPromises()
  const text=await captured[0].text(),exported=JSON.parse(text)
  expect(exported.events[0].data.snapshot.request_body).toEqual(body)
  expect(text).not.toContain('redact-me')
  expect(mocks.call.mock.calls.some(call=>call[1]==='session.start'||call[1]==='session.resume')).toBe(false)
})

it('目标统一交给Agent，后端原地关联游玩时保留原对话历史与独立预算',async()=>{
  vi.useFakeTimers({toFake:['setTimeout','clearTimeout']})
  record={conversation_id:'c1',content_package:'default',state:'idle',limits}
  events=[ev(1,'user',{message_id:'old',turn_id:'old-turn'},'先讨论新手攻略'),ev(2,'assistant_final',{message_id:'old-answer',turn_id:'old-turn',text:'已有攻略建议'})]
  const implementation=mocks.call.getMockImplementation()
  mocks.call.mockImplementation(async(id,action,values)=>{
    if(action==='conversation.message') {
      const reply=await implementation(id,action,values)
      session=[{session_id:'s1',device_id:values.device_id,content_package:'default',mode:'api',state:'starting',limits:values.game_limits}]
      record.game_session_id='s1'
      reply.conversation=structuredClone(record)
      return reply
    }
    return implementation(id,action,values)
  })
  const wrapper=await create()
  await button(wrapper,'预算').trigger('click')
  for(const field of ['模型轮数','工具次数','活动秒数','累计 Token','连续失败'])await wrapper.get(`[aria-label="游玩${field}预算"]`).setValue(0)
  await wrapper.get('[aria-label="Agent 消息"]').setValue('  完成新手教程  ')
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  await vi.advanceTimersByTimeAsync(500);await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.message',{conversation_id:'c1',device_id:'phone',message:'完成新手教程',attached_memory:[],game_limits:{max_turns:0,max_actions:0,max_seconds:0,max_tokens:0,max_failures:0}})
  expect(mocks.call.mock.calls.some(call=>call[1]==='conversation.create')).toBe(false)
  expect(mocks.call.mock.calls.some(call=>call[1]==='session.start'||call[1]==='session.resume')).toBe(false)
  expect(wrapper.get('[aria-label="聊天历史"]').element.value).toBe('c1')
  expect(wrapper.text()).toContain('先讨论新手攻略')
  expect(wrapper.text()).toContain('已有攻略建议')
  expect(wrapper.text()).toContain('完成新手教程')
  expect(wrapper.get('.game-controls').text()).toContain('phone')
  expect(wrapper.get('[aria-label="Agent 消息"]').element.value).toBe('')
})

it.each(['running','paused'])('游戏对话 %s 的后续引导统一发送，UI不代替Agent恢复设备',async(state)=>{
  record={conversation_id:'s1',game_session_id:'s1',content_package:'default',state:'idle',limits}
  session=[{session_id:'s1',device_id:'phone',content_package:'default',mode:'api',state,limits}]
  const wrapper=await create()
  expect(wrapper.find('[aria-label="消息模式"]').exists()).toBe(false)
  await wrapper.get('[aria-label="Agent 消息"]').setValue('先检查背包，再去右边')
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.message',{conversation_id:'s1',message:'先检查背包，再去右边',device_id:'phone',attached_memory:[],game_limits:limits})
  expect(mocks.call.mock.calls.some(call=>call[1]==='session.start'||call[1]==='session.resume')).toBe(false)
})

it('公开推理默认展开且不被完成步骤折叠，用户收起后增量不强行展开',async()=>{
  vi.useFakeTimers({toFake:['setTimeout','clearTimeout']})
  record={conversation_id:'c1',content_package:'default',state:'running',limits}
  events=[ev(1,'user',{message_id:'m1'},'怎么操作'),ev(2,'assistant_delta',{message_id:'a1',channel:'thinking',delta:'先**检查地图**，'}),ev(3,'tool_start',{name:'screenshot',call_id:'capture1'})]
  const wrapper=await create()
  expect(wrapper.get('.reasoning').element.open).toBe(true)
  expect(wrapper.get('.reasoning strong').text()).toBe('检查地图')
  wrapper.get('.reasoning').element.open=false
  events.push(ev(4,'assistant_delta',{message_id:'a1',channel:'thinking',delta:'再规划路径。'}),ev(5,'assistant_final',{message_id:'a1',text:'向右移动。'}),ev(6,'state',{state:'idle'}));record.state='idle'
  await vi.advanceTimersByTimeAsync(500);await flushPromises()
  expect(wrapper.get('.reasoning').element.open).toBe(false)
  expect(wrapper.get('.reasoning').text()).toContain('再规划路径')
  expect(wrapper.get('.process').element.open).toBe(false)
  expect(wrapper.get('.final-answer').text()).toContain('向右移动')
})

it('缺少推理明确显示供应商未返回，记忆草稿与整理失败直接显示并可进记忆库',async()=>{
  record={conversation_id:'c1',content_package:'default',state:'idle',limits}
  events=[ev(1,'assistant_final',{message_id:'a1',text:'这条路线完成了。'}),ev(2,'memory_staged',{memory:{id:'m1',revision:2,validation:'pending'},job_id:'j1'},'记忆草稿已保存，等待空闲整理'),ev(3,'memory_job',{job_id:'j1',memory_id:'m1',state:'running',result:{processed:1,total:3,counts:{created:1}}},'后台正在整理攻略'),ev(4,'memory_job',{job_id:'j1',memory_id:'m1',state:'failed',result:{processed:1,total:3,counts:{created:1,failed:1},error:'连接超时'}},'攻略整理失败'),ev(5,'state',{state:'idle'})]
  const wrapper=await create()
  expect(wrapper.get('.reasoning-status').text()).toContain('未记录公开推理或摘要')
  expect(wrapper.findAll('.memory-notice')).toHaveLength(2)
  expect(wrapper.text()).toContain('等待空闲整理')
  expect(wrapper.text()).toContain('待验证')
  expect(wrapper.text()).toContain('攻略整理失败')
  expect(wrapper.text()).not.toContain('后台正在整理攻略')
  expect(wrapper.get('.memory-notice.failed pre').text()).toContain('连接超时')
  expect(wrapper.get('.memory-notice.failed pre').text()).toContain('processed')
  await button(wrapper,'查看记忆').trigger('click')
  expect(wrapper.emitted('memory')).toHaveLength(1)
})
it.each(['running','paused'])('游玩 %s 引导提交独立game_limits，预算不会覆盖对话预算',async(state)=>{
  record={conversation_id:'s1',game_session_id:'s1',content_package:'default',state:'idle',limits}
  session=[{session_id:'s1',device_id:'phone',content_package:'default',mode:'api',state,limits:{...limits},usage:{turns:40,known_tokens:100000,total_tokens:null}}]
  const wrapper=await create()
  await button(wrapper,'预算').trigger('click')
  for(const field of ['模型轮数','工具次数','活动秒数','累计 Token','连续失败'])await wrapper.get(`[aria-label="游玩${field}预算"]`).setValue(0)
  await wrapper.get('[aria-label="Agent 消息"]').setValue('继续按新预算探索')
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.message',{conversation_id:'s1',message:'继续按新预算探索',device_id:'phone',attached_memory:[],game_limits:{max_turns:0,max_actions:0,max_seconds:0,max_tokens:0,max_failures:0}})
  expect(record.limits).toEqual(limits)
  await wrapper.get('[aria-label="游玩活动秒数预算"]').setValue(9)
  await wrapper.get('[aria-label="Agent 消息"]').setValue('不应提交非法预算')
  expect(wrapper.get('.composer button[type="submit"]').element.disabled).toBe(true)
})
it('同一会话对话2轮与游玩17轮分别显示，活跃游玩优先真实Session且0上限不互相混用',async()=>{
  const gameLimits={max_turns:0,max_actions:0,max_seconds:0,max_tokens:0,max_failures:0}
  record={conversation_id:'s1',game_session_id:'s1',content_package:'default',state:'idle',limits:{...limits},usage:{turns:2,actions:3,active_seconds:4,total_tokens:2000,consecutive_failures:0},game_usage:{turns:9,total_tokens:9000},game_limits:{...limits,max_tokens:50000}}
  session=[{session_id:'s1',device_id:'phone',content_package:'default',mode:'api',state:'paused',limits:gameLimits,usage:{turns:17,actions:18,active_seconds:19,total_tokens:17000,consecutive_failures:0}}]
  const wrapper=await create()
  expect(wrapper.get('.usage>summary').text()).toContain('游玩 17,000 / 不限')
  expect(wrapper.get('.usage>summary').text()).toContain('对话 2,000 / 100,000')
  expect(wrapper.get('[data-ledger="game"]').text()).toContain('模型 17 / 不限 轮')
  expect(wrapper.get('[data-ledger="chat"]').text()).toContain('模型 2 / 40 轮')
  expect(wrapper.get('[data-ledger="chat"]').text()).toContain('Token 2,000 / 100,000')
  expect(wrapper.get('[data-ledger="game"]').text()).not.toContain('9,000')
  expect(wrapper.find('[aria-label="消息模式"]').exists()).toBe(false)
  expect(wrapper.get('[data-ledger="game"]').text()).toContain('模型 17 / 不限 轮')
})

it('游戏已不在内存时从独立game快照回看，普通聊天0上限不会把游玩历史显示成无限',async()=>{
  record={conversation_id:'s1',game_session_id:'s1',content_package:'default',state:'finished',limits:{...limits,max_tokens:0},usage:{turns:2,total_tokens:2000},game_usage:{turns:17,actions:20,total_tokens:17000},game_limits:{...limits,max_turns:30,max_tokens:64000}}
  const wrapper=await create()
  expect(wrapper.get('.usage>summary').text()).toContain('游玩 17,000 / 64,000')
  expect(wrapper.get('[data-ledger="game"]').text()).toContain('模型 17 / 30 轮')
  expect(wrapper.get('[data-ledger="chat"]').text()).toContain('Token 2,000 / 不限')
  expect(wrapper.get('.usage>summary').text()).toContain('对话 2,000 / 不限')
  expect(wrapper.get('[data-ledger="game"]').text()).toContain('Token 17,000 / 64,000')
})
it('已结束游玩使用持久game快照，运行时残留的历史Session不覆盖它',async()=>{
  record={conversation_id:'s1',game_session_id:'s1',content_package:'default',state:'finished',limits:{...limits},usage:{turns:2,total_tokens:2000},game_usage:{turns:17,total_tokens:17000},game_limits:{...limits,max_tokens:64000}}
  session=[{session_id:'s1',device_id:'phone',content_package:'default',mode:'api',state:'finished',limits:{...limits,max_tokens:0},usage:{turns:99,total_tokens:99000}}]
  const wrapper=await create()
  expect(wrapper.get('.usage>summary').text()).toContain('游玩 17,000 / 64,000')
  expect(wrapper.get('[data-ledger="game"]').text()).toContain('模型 17 / 40 轮')
})

it('对话与游玩预算同时可编辑且分别提交，无需切换消息模式',async()=>{
  const originalGameLimits={...limits,max_turns:200,max_tokens:200000}
  record={conversation_id:'s1',game_session_id:'s1',content_package:'default',state:'idle',limits:{...limits},usage:{turns:2,total_tokens:2000},game_usage:{turns:17,total_tokens:17000},game_limits:originalGameLimits}
  session=[{session_id:'s1',device_id:'phone',content_package:'default',mode:'api',state:'paused',limits:originalGameLimits,usage:{turns:17,total_tokens:17000}}]
  const wrapper=await create()
  await button(wrapper,'预算').trigger('click')
  await wrapper.get('[aria-label="聊天累计 Token预算"]').setValue(0)
  await wrapper.get('[aria-label="Agent 消息"]').setValue('暂停期间解释装备')
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.message',{conversation_id:'s1',message:'暂停期间解释装备',device_id:'phone',attached_memory:[],limits:{...limits,max_tokens:0},game_limits:originalGameLimits})
  expect(wrapper.get('[data-ledger="game"]').text()).toContain('Token 17,000 / 200,000')
  expect(wrapper.get('[aria-label="游玩累计 Token预算"]').element.value).toBe('200000')
  for(const field of ['模型轮数','工具次数','活动秒数','累计 Token','连续失败'])await wrapper.get(`[aria-label="游玩${field}预算"]`).setValue(0)
  await wrapper.get('[aria-label="Agent 消息"]').setValue('改为无限预算探索')
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.message',{conversation_id:'s1',message:'改为无限预算探索',device_id:'phone',attached_memory:[],game_limits:{max_turns:0,max_actions:0,max_seconds:0,max_tokens:0,max_failures:0}})
})

it('重新打开对话时待用0预算优先填编辑框，实际游戏有限预算仍显示在账本',async()=>{
  vi.useFakeTimers({toFake:['setTimeout','clearTimeout']})
  const requested={max_turns:0,max_actions:0,max_seconds:0,max_tokens:0,max_failures:0}
  const applied={...limits,max_turns:30,max_tokens:64000}
  record={conversation_id:'c1',game_session_id:'s1',content_package:'default',state:'idle',limits,requested_game_limits:requested,game_limits:applied,game_usage:{turns:17,total_tokens:17000}}
  session=[{session_id:'s1',device_id:'phone',content_package:'default',mode:'api',state:'paused',limits:applied,usage:{turns:17,total_tokens:17000}}]
  const wrapper=await create()
  await button(wrapper,'预算').trigger('click')
  expect(wrapper.get('[aria-label="游玩累计 Token预算"]').element.value).toBe('0')
  expect(wrapper.get('[aria-label="游玩模型轮数预算"]').element.value).toBe('0')
  expect(wrapper.get('[data-ledger="game"]').text()).toContain('Token 17,000 / 64,000')
  expect(wrapper.get('[data-ledger="game"]').text()).toContain('模型 17 / 30 轮')
  expect(wrapper.get('.chat-budget').text()).toContain('修改不会自动恢复设备')
  await wrapper.get('[aria-label="Agent 消息"]').setValue('查询装备攻略，保持暂停')
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.message',{conversation_id:'c1',message:'查询装备攻略，保持暂停',device_id:'phone',attached_memory:[],game_limits:requested})
  expect(mocks.call.mock.calls.some(call=>call[1]==='session.resume')).toBe(false)
  expect(wrapper.get('[data-ledger="game"]').text()).toContain('Token 17,000 / 64,000')
  session=[];record.state='finished'
  await vi.advanceTimersByTimeAsync(500);await flushPromises()
  expect(wrapper.get('[aria-label="游玩累计 Token预算"]').element.value).toBe('0')
  expect(wrapper.get('[data-ledger="game"]').text()).toContain('Token 17,000 / 64,000')
  expect(wrapper.get('[data-ledger="game"]').text()).toContain('模型 17 / 30 轮')
})

it('新消息触发的安全暂停解释Agent接收指令，记忆查询保持暂停，继续按钮仍显式',async()=>{
  record={conversation_id:'c1',content_package:'default',game_session_id:'s1',state:'running',limits}
  session=[{session_id:'s1',device_id:'phone',content_package:'default',mode:'api',state:'paused',limits,pause_reason:{code:'agent_user_message',title:'收到新消息，Agent 正在决定后续',detail:'当前动作已完成暂停屏障，暂不执行',guidance:'查询或修改记忆保持暂停'}}]
  const implementation=mocks.call.getMockImplementation()
  mocks.call.mockImplementation(async(id,action,values)=>{if(action==='session.resume'){session[0].state='resuming';return{}};return implementation(id,action,values)})
  const wrapper=await create()
  expect(wrapper.get('.game-controls').text()).toContain('收到新消息，Agent 正在决定后续')
  expect(wrapper.get('.mode-guidance').text()).toContain('Agent 正在处理这条消息')
  await wrapper.get('[aria-label="Agent 消息"]').setValue('查一下以前的装备经验')
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  expect(mocks.call.mock.calls.some(call=>call[1]==='session.resume')).toBe(false)
  await button(wrapper,'明确继续 AI 游玩').trigger('click');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','session.resume',{session_id:'s1'})
})

it('当前设备变化仅更新消息上下文，攻略不被其他设备或外部会话阻断',async()=>{
  record={conversation_id:'c1',content_package:'default',state:'idle',limits}
  session=[{session_id:'external',device_id:'browser',content_package:'default',mode:'mcp',state:'running'}]
  const wrapper=await create()
  context.deviceId='browser';await flushPromises()
  await wrapper.get('[aria-label="Agent 消息"]').setValue('解释一下攻略术语')
  expect(wrapper.get('.composer button[type="submit"]').element.disabled).toBe(false)
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.message',{conversation_id:'c1',message:'解释一下攻略术语',device_id:'browser',attached_memory:[],game_limits:limits})
  await button(wrapper,'外部 MCP').trigger('click')
  expect(wrapper.emitted('settings').at(-1)).toEqual(['mcp'])
  expect(mocks.call.mock.calls.some(call=>call[1]==='session.start'||call[1]==='session.resume')).toBe(false)
})

it('附加自动化在有设备时仍只发送限定上下文，不启动或恢复游玩', async()=>{
  const wrapper=await create({automationContext:{script_id:'daily.yaml',run_id:'trace-1',candidate_id:'candidate-1'}})
  await wrapper.get('[aria-label="Agent 消息"]').setValue('先读取脚本和失败证据，解释原因，修改请先提出候选')
  await wrapper.get('.composer form').trigger('submit');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','conversation.message',expect.objectContaining({
    device_id:'',automation:{script_id:'daily.yaml',run_id:'trace-1',candidate_id:'candidate-1'},
  }))
  expect(wrapper.text()).toContain('候选修改需审核后保存')
  expect(mocks.call.mock.calls.some(call=>['session.start','session.resume','generation.save','generation.rollback'].includes(call[1]))).toBe(false)
  await button(wrapper,'移除').trigger('click');expect(wrapper.emitted('detach-automation')).toHaveLength(1)
})
