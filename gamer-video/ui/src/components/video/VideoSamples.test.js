// @vitest-environment happy-dom
import { mount, flushPromises } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import VideoSamples from './VideoSamples.vue'
import { videoApi } from './videoApi'
vi.mock('./videoApi', () => ({ videoApi: { listSamples: vi.fn(), recordingStatus: vi.fn(), recordingEvents: vi.fn(), mediaFrames: vi.fn(), mediaFrameNeighbors: vi.fn(), mediaFrameUrl: vi.fn(), createSample: vi.fn(), sampleUrl: vi.fn() } }))
const record = { id: 'r1', state: 'completed', event_count: 1, started_at: 'today', segments: [{ media_id: 'm1', start_us: 0, duration_us: 1000000 }] }
beforeEach(() => { vi.clearAllMocks(); videoApi.listSamples.mockResolvedValue([]); videoApi.recordingStatus.mockResolvedValue(record); videoApi.recordingEvents.mockResolvedValue([{ event_id: 'e1' }]); videoApi.mediaFrames.mockImplementation((id, q) => Promise.resolve({ frame_count: 3, current: { index: q.ptsUs === 0 ? 0 : 2, pts_us: q.ptsUs } })); videoApi.mediaFrameUrl.mockReturnValue('/frame'); videoApi.createSample.mockResolvedValue({ manifest: { id: 's1', name: 'sample', status: 'complete' } }) })
describe('screenshot and event materials', () => {
  it('creates from an ended recording without goal confirmation or video mode', async () => {
    const wrapper = mount(VideoSamples, { props: { recordingId: 'r1', recordings: [record] } }); await flushPromises()
    expect(wrapper.get('[data-testid=sample-create]').attributes('disabled')).toBeUndefined()
    expect(wrapper.find('textarea').exists()).toBe(false)
    await wrapper.get('[data-testid=sample-create]').trigger('click'); await flushPromises()
    const request=videoApi.createSample.mock.calls[0][0]
    expect(request).toMatchObject({recording_id:'r1',start_us:0,end_us:1000000})
    for(const field of ['goal','local_media','include_clips','package_id']) expect(request).not.toHaveProperty(field)
    wrapper.unmount()
  })
  it('pins END to the last real PTS using the flattened frame-neighbor response', async () => {
    videoApi.mediaFrames.mockImplementation((id, q) => Promise.resolve({ frame_count: 3, current: q.ptsUs === 0 ? { index: 0, pts_us: 0 } : null }))
    videoApi.mediaFrameNeighbors.mockResolvedValue({ index: 2, pts_us: 800000, prev: { index: 1, pts_us: 400000 }, next: null })
    const wrapper = mount(VideoSamples, { props: { recordingId: 'r1', recordings: [record] } }); await flushPromises()
    await wrapper.findAll('button').find(b => b.text() === '查看开始 / 结束画面').trigger('click'); await flushPromises()
    expect(wrapper.get('[data-testid=sample-boundaries]').text()).toContain('结束 · 0.800s')
    expect(videoApi.mediaFrameUrl).toHaveBeenCalledWith('m1', { index: 2 })
    wrapper.unmount()
  })
  it('keeps evidence independent of a surrounding package switch', async () => {
    const wrapper = mount(VideoSamples, { props: { recordingId: 'r1', recordings: [record] } }); await flushPromises()
    expect(videoApi.listSamples).toHaveBeenCalledWith()
    expect(videoApi.createSample).not.toHaveBeenCalled()
    expect(wrapper.get('[data-testid=sample-create]').attributes('disabled')).toBeUndefined()
    expect(wrapper.find('[data-testid=sample-boundaries]').exists()).toBe(false)
    wrapper.unmount()
  })
  it('explains and rejects a range beyond the saved recording without a create request', async () => {
    const wrapper = mount(VideoSamples, { props: { recordingId: 'r1', recordings: [record] } }); await flushPromises()
    await wrapper.get('[aria-label="素材结束时间"]').setValue('2')
    expect(wrapper.get('[data-testid=sample-create]').attributes('disabled')).toBeDefined()
    expect(wrapper.text()).toContain('可用录制范围')
    await wrapper.get('[data-testid=sample-create]').trigger('click'); await flushPromises()
    expect(videoApi.createSample).not.toHaveBeenCalled()
    wrapper.unmount()
  })
  it('offers the next authoring step only after completed screenshots and events, without generating anything', async () => {
    const wrapper = mount(VideoSamples, { props: { recordingId: 'r1', recordings: [record] } }); await flushPromises()
    await wrapper.get('[data-testid=sample-create]').trigger('click'); await flushPromises()
    await wrapper.findAll('button').find(button => button.text() === '去自动化使用素材').trigger('click')
    expect(wrapper.emitted('open-automation')).toHaveLength(1)
    expect(videoApi.createSample).toHaveBeenCalledTimes(1)
    wrapper.unmount()
  })
})
