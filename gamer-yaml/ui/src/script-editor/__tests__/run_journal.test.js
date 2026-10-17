// @vitest-environment happy-dom
import { afterEach, expect, it, vi } from 'vitest'
import { effectScope, ref } from 'vue'
import { flushPromises, mount } from '@vue/test-utils'
import { actionRunRows, buildRunTree, filterRunRows, flattenRunTree, runRowState, stepSummary } from '../../components/console/run-journal'
import { useRunJournal } from '../../components/console/useRunJournal'
import RunDetails from '../../components/console/RunDetails.vue'
import { api } from '../../../../../../web/src/api'
import { WORKSPACE_CONTEXT_KEY } from '../../../../../../web/src/workspace/context'
vi.mock('../../../../../../web/src/api', () => ({ api: { listRunHistory: vi.fn(), getRun: vi.fn(), getRunEvents: vi.fn(), cancelRun: vi.fn(), listExtensions: vi.fn(), getRunTrace: vi.fn() } }))
afterEach(() => { vi.useRealTimers(); vi.resetAllMocks() })
const record = (id = 'a') => ({ run_id: id, device_id: 'd', entrypoint: 'p/main.yaml', started_at: '2026-09-23T00:00:00Z', finished_at: '2026-09-23T00:00:02Z', state: 'failed', error: '未找到模板' })
const event = (id, ev, extra = {}) => ({ id, ev, time: `2026-09-23T00:00:0${id}Z`, trace: { frame_id: 0 }, ...extra })

it('opens the latest task run without replacing device history selection and retains task scope through pagination', async () => {
  const latest = { ...record('task-latest'), task_id: 'task-one' }
  const older = { ...record('task-older'), task_id: 'task-one', device_id: 'previous-device' }
  const deviceRun = record('device-selection')
  api.listRunHistory.mockImplementation((device, entrypoint, before, task) => Promise.resolve(task ? before ? [older] : Array.from({ length: 30 }, (_, i) => i ? { ...record(`task-${i}`), task_id: task } : latest) : [deviceRun]))
  api.getRun.mockImplementation(id => Promise.resolve(id === deviceRun.run_id ? deviceRun : id === older.run_id ? older : latest))
  api.getRunEvents.mockResolvedValue({ events: [], next: 0, has_more: false })
  const scope = effectScope(); let deviceJournal, taskJournal
  scope.run(() => {
    deviceJournal = useRunJournal(ref('task-scope-device'), ref(''))
    taskJournal = useRunJournal(ref('task-scope-device'), ref(''), ref(true), ref('task-one'))
  })
  try {
    await flushPromises()
    expect(taskJournal.selected.value).toBe('task-latest')
    expect(deviceJournal.selected.value).toBe('device-selection')
    expect(api.listRunHistory).toHaveBeenCalledWith(undefined, undefined, undefined, 'task-one')
    await taskJournal.loadOlder(); await flushPromises()
    expect(api.listRunHistory).toHaveBeenLastCalledWith(undefined, undefined, 'task-29', 'task-one')
    taskJournal.selected.value = older.run_id; await flushPromises()
    expect(taskJournal.record.value.device_id).toBe('previous-device')
    expect(deviceJournal.selected.value).toBe('device-selection')
  } finally { scope.stop() }
})

it('discards late history responses after switching tasks on the same device', async () => {
  let resolveFirst
  api.listRunHistory.mockImplementation((device, entrypoint, before, task) => task === 'slow-task' ? new Promise(resolve => { resolveFirst = resolve }) : Promise.resolve([{ ...record('fast-run'), task_id: task }]))
  api.getRun.mockResolvedValue({ ...record('fast-run'), task_id: 'fast-task' })
  api.getRunEvents.mockResolvedValue({ events: [], next: 0, has_more: false })
  const task = ref('slow-task'), scope = effectScope(); let journal
  scope.run(() => { journal = useRunJournal(ref('shared-device'), ref(''), ref(true), task) })
  task.value = 'fast-task'; await flushPromises()
  resolveFirst([{ ...record('stale-run'), task_id: 'slow-task' }]); await flushPromises()
  expect(journal.records.value.map(run => run.run_id)).toEqual(['fast-run'])
  expect(journal.selected.value).toBe('fast-run')
  scope.stop()
})

it('opens the failing action inspector from the task recovery button', async () => {
  const run = { ...record('task-root-failure'), task_id: 'task-inspect' }
  api.listRunHistory.mockResolvedValue([run]); api.getRun.mockResolvedValue(run)
  let finishLoading
  api.getRunEvents.mockReturnValue(new Promise(resolve => { finishLoading = resolve }))
  const wrapper = mount(RunDetails, { props: { taskId: 'task-inspect', deviceId: 'd' } })
  try {
    await flushPromises()
    expect(wrapper.find('.run-recovery button').attributes('disabled')).toBeDefined()
    expect(wrapper.find('.run-recovery button').text()).toBe('正在读取异常动作…')
    finishLoading({ events: [
    event(1, 'step_start', { path: 'run[0]', desc: '外层调用' }),
    event(2, 'step_start', { path: 'functions.test.run[0]', desc: '明确失败' }),
    event(3, 'step_end', { path: 'functions.test.run[0]', ok: false, error: '实际失败' }),
    event(4, 'step_end', { path: 'run[0]', ok: false, error: '实际失败' }),
    ], next: 4, has_more: false })
    await flushPromises()
    expect(wrapper.find('.run-recovery button').attributes('disabled')).toBeUndefined()
    await wrapper.find('.run-recovery button').trigger('click'); await flushPromises()
    expect(wrapper.find('.journal-inspector').exists()).toBe(true)
    expect(wrapper.find('.inspector-header strong').text()).toBe('明确失败')
  } finally { wrapper.unmount() }
})

it('stops the selected active run and never offers stop for an old finished record', async () => {
  const active = { ...record('cancel-live'), state: 'running', finished_at: undefined, error: undefined }
  const old = record('cancel-old')
  api.listRunHistory.mockResolvedValue([active, old])
  api.getRun.mockImplementation(async id => id === active.run_id ? active : old)
  api.getRunEvents.mockResolvedValue({ events: [], next: 0, has_more: false })
  api.cancelRun.mockImplementation(async () => { active.state = 'stopping'; return { cancelling: true } })
  const wrapper = mount(RunDetails, { props: { deviceId: 'cancel-device', liveRun: active.run_id } })
  try {
    await flushPromises()
    await wrapper.findAll('button').find(b => b.text() === '停止运行').trigger('click'); await flushPromises()
    expect(api.cancelRun).toHaveBeenCalledExactlyOnceWith('cancel-live')
    expect(wrapper.findAll('button').find(b => b.text() === '正在停止…').attributes('disabled')).toBeDefined()
    await wrapper.find('select').setValue('cancel-old'); await flushPromises()
    expect(wrapper.findAll('button').some(b => ['停止运行', '正在停止…'].includes(b.text()))).toBe(false)
  } finally { wrapper.unmount() }
})

it('keeps the chosen run when switching scripts/functions and remounting either panel', async () => {
  const runs = [record('switch-new'), { ...record('switch-old'), entrypoint: 'p#领取奖励' }]
  api.listRunHistory.mockResolvedValue(runs)
  api.getRun.mockImplementation(id => Promise.resolve(runs.find(r => r.run_id === id)))
  api.getRunEvents.mockImplementation(id => Promise.resolve({ events: [event(1, 'detail', { name: 'log', data: { message: id } })], next: 1, has_more: false }))
  let wrapper = mount(RunDetails, { props: { deviceId: 'switch-device', target: 'p/main.yaml' } })
  await flushPromises()
  await wrapper.find('select').setValue('switch-old'); await flushPromises()
  await wrapper.setProps({ target: 'p#另一个函数' }); await flushPromises()
  expect(wrapper.find('select').element.value).toBe('switch-old')
  expect(wrapper.text()).toContain('p#领取奖励')
  expect(wrapper.find('.details-stream').text()).toContain('switch-old')
  expect(api.listRunHistory).toHaveBeenCalledWith('switch-device')
  wrapper.unmount()
  wrapper = mount(RunDetails, { props: { deviceId: 'switch-device', target: 'p/another.yaml' } })
  await flushPromises()
  expect(wrapper.find('select').element.value).toBe('switch-old')
  expect(wrapper.find('.details-stream').text()).toContain('switch-old')
  wrapper.unmount()
})

it('loads earlier runs and retains them and the selection across a latest-history refresh', async () => {
  const first = Array.from({ length: 30 }, (_, i) => record(`history-${String(99-i).padStart(2, '0')}`))
  const older = record('history-01'), newer = record('history-100')
  api.listRunHistory.mockImplementation((device, target, before) => Promise.resolve(before ? [older] : first))
  api.getRun.mockImplementation(id => Promise.resolve(record(id)))
  api.getRunEvents.mockResolvedValue({ events: [], next: 0, has_more: false })
  const scope = effectScope(); let journal
  scope.run(() => { journal = useRunJournal(ref('history-device'), ref('')) })
  await flushPromises()
  expect(journal.hasOlder.value).toBe(true)
  await journal.loadOlder()
  expect(api.listRunHistory).toHaveBeenLastCalledWith('history-device', undefined, 'history-70')
  expect(journal.hasOlder.value).toBe(false)
  journal.selected.value = older.run_id; await flushPromises()
  api.listRunHistory.mockResolvedValue([newer, ...first.slice(0, 29)])
  await journal.history()
  expect(journal.records.value).toHaveLength(32)
  expect(journal.record.value.run_id).toBe(older.run_id)
  scope.stop()
})

it('continues event pagination beyond four pages without dropping the beginning or duplicating events', async () => {
  vi.useFakeTimers()
  const events = Array.from({ length: 2505 }, (_, i) => event(i + 1, 'detail', { name: 'log', data: { message: `日志 ${i + 1}` } }))
  api.listRunHistory.mockResolvedValue([record('long-run')])
  api.getRun.mockResolvedValue(record('long-run'))
  api.getRunEvents.mockImplementation((id, after) => {
    const page = events.slice(after, after + 500)
    return Promise.resolve({ events: page, next: page.at(-1)?.id || after, has_more: after + page.length < events.length })
  })
  const scope = effectScope(); let journal
  scope.run(() => { journal = useRunJournal(ref('long-device'), ref('')) })
  await flushPromises()
  expect(journal.events.value).toHaveLength(2000)
  expect(journal.hasMore.value).toBe(true)
  await vi.advanceTimersByTimeAsync(1000); await flushPromises()
  expect(journal.events.value).toHaveLength(2505)
  expect(journal.events.value[0].data.message).toBe('日志 1')
  expect(journal.events.value.at(-1).data.message).toBe('日志 2505')
  expect(journal.hasMore.value).toBe(false)
  await journal.refresh()
  expect(journal.events.value).toHaveLength(2505)
  scope.stop()
})

it('does not reselect an active run on panel remount while inspecting an older run', async () => {
  api.listRunHistory.mockResolvedValue([record('active'), record('inspecting')])
  api.getRun.mockImplementation(id => Promise.resolve(record(id)))
  api.getRunEvents.mockResolvedValue({ events: [], next: 0, has_more: false })
  const device = ref('live-device'), live = ref('active')
  let scope = effectScope(), journal
  scope.run(() => { journal = useRunJournal(device, live) })
  await flushPromises()
  journal.selected.value = 'inspecting'; await flushPromises(); scope.stop()
  scope = effectScope()
  scope.run(() => { journal = useRunJournal(device, live) })
  await flushPromises()
  expect(journal.selected.value).toBe('inspecting')
  live.value = 'next-run'; await flushPromises()
  expect(journal.selected.value).toBe('next-run')
  scope.stop()
})

it('opens finished history at the beginning and does not pull readers to the bottom on refresh', async () => {
  api.listRunHistory.mockResolvedValue([record('scroll')]); api.getRun.mockResolvedValue(record('scroll'))
  let page = 0
  api.getRunEvents.mockImplementation(() => Promise.resolve({ events: [event(++page, 'detail', { name: 'log', data: { message: `消息 ${page}` } })], next: page, has_more: false }))
  const wrapper = mount(RunDetails, { props: { deviceId: 'scroll-device', target: 'p/main.yaml' } })
  await flushPromises()
  const stream = wrapper.find('.details-stream').element
  Object.defineProperties(stream, { scrollHeight: { value: 2000 }, clientHeight: { value: 400 } })
  expect(stream.scrollTop).toBe(0)
  await wrapper.findAll('button').find(b => b.text() === '回到最新').trigger('click'); await flushPromises()
  expect(stream.scrollTop).toBe(2000)
  stream.scrollTop = 200; await wrapper.find('.details-stream').trigger('scroll')
  await wrapper.findAll('button').find(b => b.text() === '刷新').trigger('click'); await flushPromises()
  expect(stream.scrollTop).toBe(200)
  expect(wrapper.find('.details-stream').text()).toContain('消息 1')
  await wrapper.findAll('button').find(b => b.text() === '查看开头').trigger('click')
  expect(stream.scrollTop).toBe(0)
  wrapper.unmount()
})

it('keeps repeated steps distinct and attributes nested failure, parameters, logs and matching', () => {
  const tree = buildRunTree([
    event(0,'step_start',{path:'run[0]',desc:'parent'}),
    event(1,'step_start',{path:'run[0]',desc:'child',trace:{frame_id:1,parent_frame_id:0}}),
    event(2,'detail',{name:'effective_args',data:{timeout:'10s'}}),
    event(3,'vision',{template:'home',found:false}),
    event(4,'detail',{name:'log',data:{message:'test'}}),
    event(5,'step_end',{path:'run[0]',ok:false,error:'timeout',trace:{frame_id:1}}),
    event(6,'step_end',{path:'run[0]',ok:false,error:'timeout'}),
    event(7,'step_start',{path:'run[0]',desc:'again'}),
  ])
  expect(tree).toHaveLength(2)
  expect(tree[0].state).toBe('failed')
  const child = tree[0].children[0]
  expect(child.duration).toBe(4000)
  expect(child.matches).toHaveLength(1)
  expect(child.logs[0].data.message).toBe('test')
  expect(child.details[0].data.timeout).toBe('10s')
  expect(tree[1].state).toBe('running')
})

it('loads persisted terminal history without a viewer and retains the failure', async () => {
  api.listRunHistory.mockResolvedValue([record()]); api.getRun.mockResolvedValue(record())
  api.getRunEvents.mockResolvedValue({events:[event(1,'step_start',{path:'run[0]',desc:'等待主页'}),event(2,'step_end',{path:'run[0]',ok:false,error:'timeout'})],next:2,has_more:false})
  const wrapper = mount(RunDetails,{props:{deviceId:'d',target:'p/main.yaml',liveRun:''}})
  await flushPromises()
  expect(wrapper.text()).toContain('等待主页'); expect(wrapper.text()).toContain('timeout')
  expect(wrapper.text()).toContain('未找到模板')
  await wrapper.find('button').trigger('click'); expect(wrapper.emitted('edit')).toHaveLength(1)
  wrapper.unmount()
})

it('ignores stale responses after a device switch and advances the page cursor', async () => {
  vi.useFakeTimers()
  let resolveOld
  api.listRunHistory.mockImplementation(d => d === 'old' ? new Promise(resolve => { resolveOld = resolve }) : Promise.resolve([record('new')]))
  api.getRun.mockResolvedValue(record('new'))
  api.getRunEvents.mockImplementation((id, after) => Promise.resolve(after === 0 ? {events:[event(1,'run_start')], next:1,has_more:true} : {events:[event(2,'run_end',{ok:true})],next:2,has_more:false}))
  const scope = effectScope(), device = ref('old')
  let journal
  scope.run(() => { journal = useRunJournal(device,ref('')) })
  device.value = 'new'; await flushPromises()
  resolveOld([record('old')]); await flushPromises()
  expect(journal.selected.value).toBe('new')
  expect(journal.events.value.map(e => e.id)).toEqual([1,2])
  expect(api.getRunEvents).toHaveBeenCalledWith('new',1)
  scope.stop()
})

it('retains iteration context for every repeated child instead of the last loop iteration', () => {
  const rows = flattenRunTree(buildRunTree([
    event(0, 'step_start', { path: 'run[0]', desc: '每日任务' }),
    event(1, 'detail', { name: 'iteration', data: { iteration: 1, total: 2 } }),
    event(2, 'step_start', { path: 'run[0].do[0]', desc: '领取奖励' }),
    event(3, 'step_end', { path: 'run[0].do[0]', ok: true }),
    event(4, 'detail', { name: 'iteration', data: { iteration: 2, total: 2 } }),
    event(5, 'step_start', { path: 'run[0].do[0]', desc: '领取奖励' }),
    event(6, 'step_end', { path: 'run[0].do[0]', ok: true }),
    event(7, 'step_end', { path: 'run[0]', ok: true }),
  ]))
  expect(rows.map(row => row.node.id)).toEqual([0, 2, 5])
  expect(rows[1].context).toBe('每日任务 › 第 1 轮')
  expect(rows[2].context).toBe('每日任务 › 第 2 轮')
})

it('finds deep errors and warnings without treating a template miss as failure', () => {
  const rows = flattenRunTree(buildRunTree([
    event(0, 'step_start', { path: 'run[0]', desc: '主流程' }),
    event(1, 'step_start', { path: 'run[0].do[0]', desc: '重试' }),
    event(2, 'vision', { template: '主页', found: false }),
    event(3, 'step_end', { path: 'run[0].do[0]', ok: true }),
    event(4, 'step_start', { path: 'run[0].do[1]', desc: '继续' }),
    event(5, 'detail', { name: 'log', data: { level: 'warn', message: '等待太久' } }),
    event(6, 'step_end', { path: 'run[0].do[1]', ok: true }),
    event(7, 'step_start', { path: 'run[0].do[2]', desc: '领取' }),
    event(8, 'step_end', { path: 'run[0].do[2]', ok: false, error: '超时' }),
  ]))
  expect(filterRunRows(rows).map(r => r.node.id)).toEqual([1, 4, 7])
  expect(filterRunRows(rows, { issuesOnly: true }).map(r => r.node.id)).toEqual([4, 7])
  expect(filterRunRows(rows, { query: '主页' }).map(r => r.node.id)).toEqual([1])
  expect(filterRunRows(rows, { query: '等待太久' }).map(r => r.node.id)).toEqual([4])
  expect(filterRunRows(rows, { issuesOnly: true, query: '超时' }).map(r => r.node.id)).toEqual([7])
})

it('shows a deep failure once in a flat table and inspects its context outside the list', async () => {
  const events = Array.from({ length: 8 }, (_, i) => event(i, 'step_start', {
    path: 'run[0]', desc: `调用 ${i}`, trace: { frame_id: i, source: { function: `函数${i}` } },
  }))
  const failure = event(8, 'step_end', { path: 'run[0]', ok: false, error: '目标未出现', trace: { frame_id: 7, source: { function: '函数7' } } })
  events.push(failure)
  api.listRunHistory.mockResolvedValue([record()]); api.getRun.mockResolvedValue(record())
  api.getRunEvents.mockResolvedValue({ events, next: 8, has_more: false })
  const wrapper = mount(RunDetails, { props: { deviceId: 'd', target: 'p/main.yaml' } })
  await flushPromises()
  expect(wrapper.findAll('.journal-table tbody > tr')).toHaveLength(1)
  expect(wrapper.find('.row-action').text()).toBe('调用 7')
  await wrapper.findAll('.view-switch button').find(b => b.text() === '全部步骤').trigger('click')
  expect(wrapper.findAll('.journal-table tbody > tr')).toHaveLength(8)
  expect(wrapper.find('.fold-button').exists()).toBe(false)
  expect(wrapper.find('.journal-table table').exists()).toBe(false)
  expect(wrapper.findAll('.journal-row').every(row => !row.attributes('style'))).toBe(true)
  await wrapper.find('input[type="search"]').setValue('目标未出现')
  expect(wrapper.findAll('.journal-row')).toHaveLength(1)
  await wrapper.find('.row-action').trigger('click')
  expect(wrapper.find('.details-stream .journal-inspector').exists()).toBe(false)
  expect(wrapper.find('.journal-inspector').text()).toContain('调用 0 › 调用 1')
  expect(wrapper.find('.journal-inspector details').exists()).toBe(false)
  await wrapper.findAll('.inspector-header button').find(b => b.text() === '定位步骤').trigger('click')
  expect(wrapper.emitted('locate')[0][0]).toEqual(failure)
  await wrapper.find('[aria-label="关闭记录详情"]').trigger('click')
  expect(wrapper.find('.journal-inspector').exists()).toBe(false)
  wrapper.unmount()
})

it('retains a flow record with its own observations or a failure outside its completed child', () => {
  const rows = flattenRunTree(buildRunTree([
    event(0, 'step_start', { path: 'run[0]', desc: '循环' }),
    event(1, 'step_start', { path: 'run[0].do[0]', desc: '点击' }),
    event(2, 'step_end', { path: 'run[0].do[0]', ok: true }),
    event(3, 'step_end', { path: 'run[0]', ok: false, error: '循环失败' }),
  ]))
  expect(actionRunRows(rows).map(r => r.node.id)).toEqual([0, 1])
  rows[0].node.state = 'success'
  expect(actionRunRows(rows).map(r => r.node.id)).toEqual([1])
  rows[0].node.logs.push(event(4, 'detail', { name: 'log', data: { level: 'warn', message: '流程警告' } }))
  expect(actionRunRows(rows).map(r => r.node.id)).toEqual([0, 1])
  expect(runRowState(rows[0].node)).toBe('warning')
})

it('keeps full parameters, match attempts and earlier logs in one inspector without expanding rows', async () => {
  const events = [
    event(1, 'step_start', { path: 'run[0]', desc: '检查主页' }),
    event(2, 'detail', { name: 'effective_args', data: { timeout: '10s' } }),
    event(3, 'vision', { template: 'home.png', found: false }),
    event(4, 'vision', { template: 'home.png', found: true, score: 0.96 }),
    event(5, 'detail', { name: 'log', data: { message: '第一条日志' } }),
    event(6, 'detail', { name: 'log', data: { message: '第二条日志' } }),
    event(7, 'step_end', { path: 'run[0]', ok: true }),
  ]
  api.listRunHistory.mockResolvedValue([record('inspector')]); api.getRun.mockResolvedValue(record('inspector'))
  api.getRunEvents.mockResolvedValue({ events, next: 7, has_more: false })
  const wrapper = mount(RunDetails, { props: { deviceId: 'inspector-device' } })
  await flushPromises()
  await wrapper.find('.row-action').trigger('click')
  expect(wrapper.find('.journal-inspector').text()).toContain('第一条日志')
  expect(wrapper.find('.journal-inspector').text()).toContain('第二条日志')
  await wrapper.findAll('.inspector-tab')[1].trigger('click')
  expect(wrapper.find('.inspector-body').text()).toContain('10s')
  await wrapper.findAll('.inspector-tab')[2].trigger('click')
  expect(wrapper.findAll('.match-record')).toHaveLength(2)
  expect(wrapper.find('.journal-table').find('pre').exists()).toBe(false)
  wrapper.unmount()
})

it('summarizes false and zero return values without treating them as errors', () => {
  for (const value of [false, 0]) {
    const rows = flattenRunTree(buildRunTree([
      event(1, 'step_start', { path: 'run[0]', desc: '比较' }),
      event(2, 'detail', { name: 'result', data: { value, as: 'result' } }),
      event(3, 'step_end', { path: 'run[0]', ok: true }),
    ]))
    expect(stepSummary(rows[0].node)).toBe(`result = ${value}`)
    expect(runRowState(rows[0].node)).toBe('success')
  }
})
it('a failed run offers anomaly review while AI actions remain unavailable across package boundaries', async () => {
  const failed = { ...record('package-boundary'), runner_id: 'gamer-yaml', content_package: 'p' }
  api.listRunHistory.mockResolvedValue([failed]); api.getRun.mockResolvedValue(failed)
  api.getRunEvents.mockResolvedValue({ events: [event(1,'step_start',{path:'run[0]',desc:'等待模板'}),event(2,'step_end',{path:'run[0]',ok:false})], next:2, has_more:false })
  api.listExtensions.mockResolvedValue({ extensions: [{ id:'gamer-ai', state:'running' }] })
  api.getRunTrace.mockResolvedValue({ enabled:false, status:'disabled', images:[], gaps:[] })
  const wrapper = mount(RunDetails, { props:{deviceId:'scope-device'}, global:{provide:{[WORKSPACE_CONTEXT_KEY]:{getSnapshot:()=>({currentPackageId:'other'})}}} })
  try {
    await flushPromises()
    const button = label => wrapper.findAll('button').find(item=>item.text()===label)
    expect(button('复制 Agent 引用')).toBeDefined()
    expect(button('交给 AI 分析')).toBeUndefined()
    expect(button('查看失败版本').attributes('disabled')).toBeDefined()
    await button('查看异常动作').trigger('click'); await flushPromises()
    expect(wrapper.get('.issue-filter input').element.checked).toBe(true)
    expect(api.cancelRun).not.toHaveBeenCalled()
  } finally {wrapper.unmount()}
})
