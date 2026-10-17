// @vitest-environment happy-dom
import { mount, flushPromises } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'
import Panel from './AutomationGenerationPanel.vue'
import { requestRunSourceReview } from './automationContextBridge'
import { pluginMessageChannel } from '../../../../../../web/src/workspace/plugin-messages'
import { api } from '../../../../../../web/src/api'
vi.mock('../../../../../../web/src/api', () => ({ api: { callExtension: vi.fn(), listTemplates: vi.fn(), getRun: vi.fn(), getRunTrace: vi.fn() } }))
const wrappers=[]
afterEach(() => { wrappers.forEach(w=>w.unmount());wrappers.length=0;vi.clearAllMocks();localStorage.clear(); const request=pluginMessageChannel('gamer-yaml:open-generation'); request.repairRun=null;request.scriptId='';request.packageId='';request.seq=0 })
function fixtures() {
  const doc={script_id:'daily.yaml',head_version:'0.1',published_version:null,job:null,versions:[{version:'0.1',yaml:'run: []\n',published:false,diagnostics:[],instruction:'first'}]}
  api.listTemplates.mockResolvedValue([{name:'done.png'}])
  api.callExtension.mockImplementation(async (_id,action,values)=>{
    if(action==='authoring.list')return{scripts:[{script_id:'daily.yaml',head_version:'0.1'}]}
    if(action==='generation.readiness')return{ready:true}
    if(action==='sample.list')return{samples:[]}
    if(action==='authoring.get')return{script:doc}
    if(action==='authoring.generate')return{script:{...doc,job:{id:'j',state:'generating'}}}
    if(action==='authoring.publish')return{script:{...doc,published_version:'1.0',head_version:'1.0',versions:[...doc.versions,{...doc.versions[0],version:'1.0',published:true}]}}
    return{}
  })
  return doc
}
describe('external Agent work drafts and preserved versions',()=>{
  it('shows saved source while the material list and model readiness are still pending',async()=>{
    const doc=fixtures();let finishMaterials,finishModel
    const materials=new Promise(resolve=>{finishMaterials=resolve})
    const model=new Promise(resolve=>{finishModel=resolve})
    api.callExtension.mockImplementation(async (_id,action)=>{
      if(action==='authoring.list')return{scripts:[{script_id:'daily.yaml',head_version:'0.1'}]}
      if(action==='authoring.get')return{script:doc}
      if(action==='sample.list')return materials
      if(action==='generation.readiness')return model
      return{}
    })
    const wrapper=mount(Panel,{props:{packageId:'pkg'}});wrappers.push(wrapper);await flushPromises()
    expect(wrapper.get('[aria-label="脚本 YAML"]').element.value).toBe(doc.versions[0].yaml)
    expect(wrapper.text()).toContain('正在读取素材列表')
    expect(wrapper.text()).not.toContain('暂无素材')
    finishMaterials({samples:[{id:'material',name:'late material',status:'complete'}]});finishModel({ready:true});await flushPromises()
    expect(wrapper.text()).toContain('late material')
    expect(wrapper.text()).not.toContain('正在读取素材列表')
    expect(api.callExtension.mock.calls.some(c=>['authoring.generate','authoring.run','authoring.save','authoring.publish'].includes(c[1]))).toBe(false)
  })
  it('reopens the saved work draft and latest source after reload without generating or publishing',async()=>{
    const doc=fixtures()
    localStorage.setItem('gamer-yaml.authoring.pkg',JSON.stringify({scriptId:'daily.yaml'}))
    const wrapper=mount(Panel,{props:{packageId:'pkg'}});wrappers.push(wrapper);await flushPromises()
    expect(wrapper.get('[aria-label="指定脚本"]').element.value).toBe('daily.yaml')
    expect(wrapper.get('[aria-label="脚本 YAML"]').element.value).toBe(doc.versions[0].yaml)
    expect(api.callExtension.mock.calls.some(c=>['authoring.generate','authoring.run','authoring.save','authoring.publish'].includes(c[1]))).toBe(false)
  })
  it('selects an existing revision when the old selection is unavailable instead of opening an empty draft',async()=>{
    fixtures()
    api.callExtension.mockImplementation(async (_id,action)=>{
      if(action==='authoring.list')return{scripts:[{script_id:'empty.yaml',head_version:''},{script_id:'daily.yaml',head_version:'0.1'}]}
      if(action==='authoring.get')return{script:{script_id:'daily.yaml',head_version:'0.1',versions:[{version:'0.1',yaml:'# saved source\nrun: []\n',diagnostics:[]}]}}
      if(action==='generation.readiness')return{ready:true}
      if(action==='sample.list')return{samples:[]}
      return{}
    })
    localStorage.setItem('gamer-yaml.authoring.pkg',JSON.stringify({scriptId:'removed.yaml'}))
    const wrapper=mount(Panel,{props:{packageId:'pkg'}});wrappers.push(wrapper);await flushPromises()
    expect(wrapper.get('[aria-label="指定脚本"]').element.value).toBe('daily.yaml')
    expect(wrapper.get('[aria-label="脚本 YAML"]').element.value).toContain('# saved source')
    expect(api.callExtension.mock.calls.some(c=>c[1]==='authoring.create')).toBe(false)
  })
  it('opens the exact failed private version and preserves original materials without starting work',async()=>{
    const doc=fixtures();doc.versions[0].material_refs=[{plugin_id:'gamer-video',sample_id:'original',content_sha256:'hash'}]
    doc.head_version='0.2';doc.versions.push({...doc.versions[0],version:'0.2',yaml:'# newer source\nrun: []\n'})
    api.getRun.mockResolvedValue({state:'failed',entrypoint:'pkg/daily.yaml',runner_id:'gamer-yaml-version'})
    api.getRunTrace.mockResolvedValue({snapshot:{_script_revision:'0.1',_source_files:{'automations/daily.yaml':doc.versions[0].yaml}}})
    requestRunSourceReview('pkg','daily.yaml','failed-original')
    const wrapper=mount(Panel,{props:{packageId:'pkg'}});wrappers.push(wrapper);await flushPromises()
    expect(wrapper.get('[aria-label="脚本版本"]').element.value).toBe('0.1')
    expect(wrapper.get('[aria-label="脚本 YAML"]').element.value).toBe(doc.versions[0].yaml)
    expect(wrapper.text()).toContain('original')
    expect(api.callExtension.mock.calls.some(c=>['authoring.generate','generation.readiness','authoring.run','authoring.save','authoring.publish'].includes(c[1]))).toBe(false)
  })
  it('rejects unavailable failed source instead of replacing it with the latest editor',async()=>{
    fixtures();api.getRun.mockResolvedValue({state:'failed',entrypoint:'pkg/daily.yaml'})
    api.getRunTrace.mockResolvedValue({snapshot:{_source_files:{'automations/daily.yaml':'# unrelated source\nrun: []\n'}}})
    requestRunSourceReview('pkg','daily.yaml','missing-source')
    const wrapper=mount(Panel,{props:{packageId:'pkg'}});wrappers.push(wrapper);await flushPromises()
    expect(wrapper.get('[role="alert"]').text()).toContain('不能用当前源码替代')
    expect(api.callExtension.mock.calls.some(c=>c[1]==='authoring.generate')).toBe(false)
  })
  it('saves an explicit source edit as a new version without changing historical source or publishing',async()=>{
    const doc=fixtures(), implementation=api.callExtension.getMockImplementation()
    api.callExtension.mockImplementation(async (id,action,values)=>action==='authoring.save'?{script:{...doc,head_version:'0.2',versions:[...doc.versions,{...doc.versions[0],version:'0.2',yaml:values.yaml}]}}:implementation(id,action,values))
    const wrapper=mount(Panel,{props:{packageId:'pkg'}});wrappers.push(wrapper);await flushPromises()
    await wrapper.get('[aria-label="脚本 YAML"]').setValue('# revised\nrun: []\n')
    await wrapper.findAll('button').find(b=>b.text()==='保存修改为小版本').trigger('click');await flushPromises()
    expect(api.callExtension).toHaveBeenCalledWith('gamer-yaml','authoring.save',expect.objectContaining({expected_version:'0.1',version:'0.1',yaml:'# revised\nrun: []\n'}))
    expect(doc.versions[0].yaml).toBe('run: []\n')
    expect(wrapper.get('[aria-label="脚本版本"]').element.value).toBe('0.2')
    expect(api.callExtension.mock.calls.some(c=>['authoring.generate','generation.readiness','authoring.run','authoring.publish'].includes(c[1]))).toBe(false)
  })
  it('publishes the selected revision without another model call',async()=>{
    fixtures();const wrapper=mount(Panel,{props:{packageId:'pkg'}});wrappers.push(wrapper);await flushPromises()
    await wrapper.get('[aria-label="指定脚本"]').setValue('daily.yaml');await flushPromises()
    await wrapper.findAll('button').find(b=>b.text()==='发布到脚本库').trigger('click');await flushPromises()
    expect(api.callExtension).toHaveBeenCalledWith('gamer-yaml','authoring.publish',expect.objectContaining({script_id:'daily.yaml',version:'0.1'}))
    expect(wrapper.text()).toContain('已发布 1.0')
    expect(api.callExtension.mock.calls.some(c=>c[1]==='authoring.generate')).toBe(false)
  })
  it('shows a failed generation after reopening without losing previous revisions',async()=>{
    const doc=fixtures();doc.job={state:'failed',error:'素材缺少完成画面'}
    const wrapper=mount(Panel,{props:{packageId:'pkg'}});wrappers.push(wrapper);await flushPromises()
    await wrapper.get('[aria-label="指定脚本"]').setValue('daily.yaml');await flushPromises()
    expect(wrapper.text()).toContain('素材缺少完成画面')
    expect(wrapper.get('[aria-label="脚本 YAML"]').element.value).toBe('run: []\n')
    expect(api.callExtension.mock.calls.some(c=>c[1]==='authoring.generate')).toBe(false)
  })
})
