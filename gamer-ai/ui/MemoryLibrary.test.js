import {beforeEach,afterEach,expect,it,vi} from 'vitest'
import {mount,flushPromises} from '@vue/test-utils'
const mocks=vi.hoisted(()=>({call:vi.fn()}))
vi.mock('../../../web/src/api',()=>({api:{callExtension:mocks.call}}))
import MemoryLibrary from './MemoryLibrary.vue'
let wrappers,memory,jobs
const button=(wrapper,label)=>wrapper.findAll('button').find(item=>item.text()===label)
async function create(){const wrapper=mount(MemoryLibrary,{props:{packageId:'default'}});wrappers.push(wrapper);await flushPromises();return wrapper}
beforeEach(()=>{
  wrappers=[];jobs=[];memory={id:'m1',title:'背包流程',body:'前提\n新步骤',revision:2,version:'hash2',kind:'procedure',tags:['背包'],status:'active',validation:'pending',game_version:'unknown',sources:[{source_id:'source1',source_revision:1}],protected_fields:['body']}
  mocks.call.mockReset().mockImplementation(async(_,action,values)=>{
    if(action==='memory.list')return{items:[memory],total:1,diagnostics:[]}
    if(action==='memory.search')return{items:[{...memory,summary:'背包入口'}],retrieval:{keyword:true,semantic:false,degraded_reason:'未配置向量服务',pending_vectors:1}}
    if(action==='memory.get')return{memory:values.revision===1?{...memory,body:'前提\n旧步骤',revision:1,version:'hash1'}:memory,revision:memory.revision,version:memory.version}
    if(action==='memory.history')return{items:[{...memory,revision:1}],total:1}
    if(action==='memory.source.get')return{source:{id:'source1',text:'来源攻略原文',revision:1}}
    if(action==='memory.jobs')return{items:jobs}
    if(action==='memory.index'||action==='memory.rebuild')return{keyword_ready:true,semantic_ready:false,total_memories:1,total_chunks:2,pending_vectors:1}
    if(action==='memory.import'){jobs=[{id:'j1',title:values.title,status:'pending',processed:0,total:2,limits:values.limits,usage:{known_tokens:1000,total_tokens:null,turns:1,actions:2,active_seconds:3},counts:{created:0,updated:0,merged:0,retained:0,failed:0}}];return{job_id:'j1',status:'pending',chunks:2}}
    if(action.startsWith('memory.job.')){jobs[0].status=action.endsWith('pause')?'paused':action.endsWith('resume')?'running':'cancelled';return{ok:true}}
    throw new Error(`Unexpected ${action}`)
  })
})
afterEach(()=>{wrappers.forEach(wrapper=>wrapper.unmount());vi.useRealTimers();vi.restoreAllMocks()})
it('只读搜索含pending草稿，查看全文来源历史差异，并固定revision附到对话',async()=>{
  const wrapper=await create()
  await wrapper.get('[aria-label="搜索记忆"]').setValue('背包')
  await wrapper.get('form.search').trigger('submit');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','memory.search',{content_package:'default',query:'背包',limit:30,validation:'any',include_inactive:false,mode:'hybrid'})
  expect(wrapper.text()).toContain('未配置向量服务')
  await wrapper.get('.memory-list button').trigger('click');await flushPromises()
  expect(wrapper.get('[aria-label="记忆全文"]').text()).toContain('新步骤')
  expect(wrapper.text()).toContain('人工指定保护')
  await button(wrapper,'读取来源原文').trigger('click');await flushPromises()
  expect(wrapper.get('[aria-label="来源全文"]').text()).toBe('来源攻略原文')
  await wrapper.get('[aria-label="比较修订版本"]').setValue('1');await flushPromises()
  expect(wrapper.get('[aria-label="修订差异"]').text()).toContain('旧步骤')
  expect(wrapper.get('[aria-label="修订差异"]').text()).toContain('新步骤')
  await button(wrapper,'附到对话').trigger('click')
  expect(wrapper.emitted('attach')[0][0]).toEqual({id:'m1',revision:2,title:'背包流程',content_package:'default'})
  expect(wrapper.find('textarea').exists()).toBe(false)
  expect(mocks.call.mock.calls.some(call=>/memory\.(update|delete|restore|disable)/.test(call[1]))).toBe(false)
})
it('来源修订后优先显示有效待验证状态，保留原验证标记与历史来源警告',async()=>{
  memory.validation='verified';memory.effective_validation='pending'
  memory.source_conflicts=[{source_id:'source1',cited_revision:1,current_revision:2,deleted:true,reason:'source_changed_requires_review'}]
  const implementation=mocks.call.getMockImplementation()
  mocks.call.mockImplementation(async(id,action,values)=>{
    if(action==='memory.get')return{memory:{...memory,source_conflicts:undefined},source_conflicts:memory.source_conflicts}
    if(action==='memory.source.get')return{source:{id:'source1',text:'已删除来源的旧原文',revision:1},current_revision:2,current_deleted:true,changed_since_reference:true}
    return implementation(id,action,values)
  })
  const wrapper=await create()
  expect(wrapper.get('.memory-list').text()).toContain('待验证')
  expect(wrapper.get('.memory-list').text()).toContain('资料需复核')
  expect(wrapper.get('.memory-list').text()).not.toContain('来源已验证')
  await wrapper.get('.memory-list button').trigger('click');await flushPromises()
  expect(wrapper.get('.memory-full').text()).toContain('保存的验证标记：来源已验证')
  expect(wrapper.get('.memory-full').text()).toContain('引用修订 r1 → 当前 r2（已删除）')
  await button(wrapper,'读取来源原文').trigger('click');await flushPromises()
  expect(wrapper.get('.memory-full').text()).toContain('这是引用的历史原文')
  expect(wrapper.get('[aria-label="来源全文"]').text()).toBe('已删除来源的旧原文')
})
it('有独立有效来源时保留有效验证状态，仍展示变更来源的复核警告',async()=>{
  memory.validation='verified';memory.effective_validation='verified'
  memory.source_conflicts=[{source_id:'source1',cited_revision:1,current_revision:2,deleted:false}]
  const wrapper=await create()
  expect(wrapper.get('.memory-list').text()).toContain('来源已验证')
  expect(wrapper.get('.memory-list').text()).toContain('资料需复核')
  expect(wrapper.get('.memory-list').text()).toContain('其他独立来源仍支持当前内容')
})
it('MD导入只暂存原文，展示作业真实进度并可暂停继续取消',async()=>{
  const wrapper=await create(),input=wrapper.get('[aria-label="选择攻略文件"]')
  for(const label of ['模型轮数','工具次数','活动秒数','累计 Token','连续失败'])await wrapper.get(`[aria-label="合并${label}预算"]`).setValue(0)
  Object.defineProperty(input.element,'files',{configurable:true,value:[new File(['# 背包\n点击背包后观察道具'], '攻略.md',{type:'text/markdown'})]})
  await input.trigger('change')
  await button(wrapper,'暂存并交给 AI 合并').trigger('click');await flushPromises();await new Promise(resolve=>setTimeout(resolve,30));await flushPromises()
  const values=mocks.call.mock.calls.find(call=>call[1]==='memory.import')?.[2]
  expect(values).toMatchObject({content_package:'default',filename:'攻略.md',title:'攻略',text:'# 背包\n点击背包后观察道具'})
  expect(values.operation_id).toBeTruthy()
  expect(values.limits).toEqual({max_turns:0,max_actions:0,max_seconds:0,max_tokens:0,max_failures:0})
  expect(wrapper.get('.job-list').text()).toContain('至少 1,000 / 不限')
  expect(wrapper.get('.job-list').text()).toContain('待处理 · 0 / 2')
  await button(wrapper,'暂停合并').trigger('click');await flushPromises()
  expect(wrapper.get('.job-list').text()).toContain('已暂停')
  await button(wrapper,'继续合并').trigger('click');await flushPromises()
  await wrapper.get('.job-list form').trigger('submit');await flushPromises()
  expect(mocks.call.mock.calls.find(call=>call[1]==='memory.job.resume')[2].limits).toEqual(values.limits)
  expect(wrapper.get('.job-list').text()).toContain('处理中')
  await button(wrapper,'取消作业').trigger('click');await flushPromises()
  expect(wrapper.get('.job-list').text()).toContain('已取消')
})
it('拒绝不支持的文件，切包前发起的正文响应不会显示到另一包',async()=>{
  const wrapper=await create(),input=wrapper.get('[aria-label="选择攻略文件"]')
  Object.defineProperty(input.element,'files',{configurable:true,value:[new File(['x'], '攻略.pdf')]});await input.trigger('change')
  expect(wrapper.text()).toContain('仅支持每个不超过 1 MiB 的 MD/TXT')
  expect(button(wrapper,'暂存并交给 AI 合并').element.disabled).toBe(true)
  const implementation=mocks.call.getMockImplementation();let reply
  mocks.call.mockImplementation((id,action,values)=>action==='memory.get'?new Promise(resolve=>reply=resolve):implementation(id,action,values))
  await wrapper.get('.memory-list button').trigger('click')
  await wrapper.setProps({packageId:'other'});await flushPromises()
  reply({memory});await flushPromises()
  expect(wrapper.find('[aria-label="记忆全文"]').exists()).toBe(false)
})
it('记忆作业上下文按after_seq只读分页，保留system及实际工具而不恢复作业',async()=>{
  jobs=[{id:'j1',title:'历史游玩整理',status:'paused',processed:1,total:3,counts:{}}]
  const implementation=mocks.call.getMockImplementation()
  const event=seq=>({seq,kind:'prompt_snapshot',at:'2026-10-04T07:00:00+08:00',data:{scope:'import',job_id:'j1',snapshot:{protocol:'responses',model:'glm-test',request_body:{input:[{role:'system',content:`真实合并提示词${seq}`}],tools:[{type:'function',name:'memory_import_finish'}]}}}})
  mocks.call.mockImplementation(async(id,action,values)=>action==='memory.job.prompts'?{events:[event(values.after_seq===0?1:2)],total:2,latest_seq:2,next_after_seq:values.after_seq===0?1:null}:implementation(id,action,values))
  const wrapper=await create()
  await button(wrapper,'查看请求上下文').trigger('click');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','memory.job.prompts',{content_package:'default',job_id:'j1',after_seq:0,limit:80})
  expect(wrapper.get('[aria-label="记忆整理请求上下文"]').text()).toContain('真实合并提示词1')
  expect(wrapper.text()).toContain('后台记忆整理不提供设备操作工具')
  await button(wrapper,'加载后续请求').trigger('click');await flushPromises()
  expect(mocks.call).toHaveBeenCalledWith('gamer-ai','memory.job.prompts',{content_package:'default',job_id:'j1',after_seq:1,limit:80})
  expect(wrapper.text()).toContain('真实合并提示词2')
  expect(wrapper.text()).toContain('真实合并提示词1')
  expect(wrapper.text()).toContain('只读记录 2 / 2')
  expect(button(wrapper,'加载后续请求')).toBeUndefined()
  expect(mocks.call.mock.calls.some(call=>/memory\.job\.(resume|pause|cancel)/.test(call[1]))).toBe(false)
})
it('记忆请求快照读取跨包响应丢弃，旧作业没有快照时明确不能补回',async()=>{
  jobs=[{id:'j1',title:'旧作业',status:'completed',processed:1,total:1,counts:{}}]
  const implementation=mocks.call.getMockImplementation();let reply
  mocks.call.mockImplementation((id,action,values)=>action==='memory.job.prompts'?new Promise(resolve=>reply=resolve):implementation(id,action,values))
  const wrapper=await create()
  await button(wrapper,'查看请求上下文').trigger('click')
  await wrapper.setProps({packageId:'other'});await flushPromises()
  reply({events:[{seq:1,data:{snapshot:{request_body:{input:[{role:'system',content:'不得泄漏到其他包'}]}}}}],total:1,next_after_seq:null})
  await flushPromises()
  expect(wrapper.find('[aria-label="记忆整理请求上下文"]').exists()).toBe(false)
  expect(wrapper.text()).not.toContain('不得泄漏到其他包')
  mocks.call.mockImplementation(async(id,action,values)=>action==='memory.job.prompts'?{events:[],total:0,latest_seq:0,next_after_seq:null}:implementation(id,action,values))
  await button(wrapper,'查看请求上下文').trigger('click');await flushPromises()
  expect(wrapper.text()).toContain('旧请求无法补回')
})
