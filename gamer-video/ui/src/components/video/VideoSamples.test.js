// @vitest-environment happy-dom
import { mount, flushPromises } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import VideoSamples from './VideoSamples.vue'
import { videoApi } from './videoApi'
vi.mock('./videoApi', () => ({ videoApi: { listSamples: vi.fn(), recordingStatus: vi.fn(), recordingEvents: vi.fn(), mediaFrames: vi.fn(), mediaFrameNeighbors: vi.fn(), mediaFrameUrl: vi.fn(), createSample: vi.fn(), sampleUrl: vi.fn() } }))
const record = { id: 'r1', state: 'completed', event_count: 1, started_at: 'today', segments: [{ media_id: 'm1', start_us: 0, duration_us: 1000000 }] }
beforeEach(() => { vi.clearAllMocks(); videoApi.listSamples.mockResolvedValue([]); videoApi.recordingStatus.mockResolvedValue(record); videoApi.recordingEvents.mockResolvedValue([{ event_id: 'e1' }]); videoApi.mediaFrames.mockImplementation((id, q) => Promise.resolve({ frame_count: 3, current: { index: q.ptsUs === 0 ? 0 : 2, pts_us: q.ptsUs } })); videoApi.mediaFrameUrl.mockReturnValue('/frame'); videoApi.createSample.mockResolvedValue({ manifest: { id: 's1', name: 'sample', status: 'complete' } }) })
describe('explicit goal evidence', () => {
  it('does not infer END confirmation from a stopped recording', async () => {
    const wrapper = mount(VideoSamples, { props: { packageId: 'pkg', recordingId: 'r1', recordings: [record] } }); await flushPromises()
    expect(wrapper.get('[data-testid=sample-create]').attributes('disabled')).toBeDefined()
    await wrapper.findAll('button').find(b => b.text() === '查看 START / END').trigger('click'); await flushPromises()
    expect(wrapper.get('[data-testid=sample-goal-confirmed]').attributes('disabled')).toBeDefined()
    for (const image of wrapper.findAll('img')) await image.trigger('load')
    await wrapper.get('textarea').setValue('reward is claimed')
    await wrapper.get('[data-testid=sample-goal-confirmed]').setValue(true)
    expect(wrapper.get('[data-testid=sample-create]').attributes('disabled')).toBeUndefined()
    await wrapper.get('[data-testid=sample-create]').trigger('click'); await flushPromises()
    expect(videoApi.createSample).toHaveBeenCalledTimes(1)
    expect(videoApi.createSample.mock.calls[0][0].goal).toEqual({ description: 'reward is claimed', confirmed: true })
    wrapper.unmount()
  })
  it('pins END to the last real PTS using the flattened frame-neighbor response', async () => {
    videoApi.mediaFrames.mockImplementation((id, q) => Promise.resolve({ frame_count: 3, current: q.ptsUs === 0 ? { index: 0, pts_us: 0 } : null }))
    videoApi.mediaFrameNeighbors.mockResolvedValue({ index: 2, pts_us: 800000, prev: { index: 1, pts_us: 400000 }, next: null })
    const wrapper = mount(VideoSamples, { props: { packageId: 'pkg', recordingId: 'r1', recordings: [record] } }); await flushPromises()
    await wrapper.findAll('button').find(b => b.text() === '查看 START / END').trigger('click'); await flushPromises()
    expect(wrapper.get('[data-testid=sample-boundaries]').text()).toContain('END · 0.800s')
    expect(videoApi.mediaFrameUrl).toHaveBeenCalledWith('m1', { index: 2 })
    wrapper.unmount()
  })
  it('clears confirmation and stale results on package switch', async () => {
    const wrapper = mount(VideoSamples, { props: { packageId: 'pkg', recordingId: 'r1', recordings: [record] } }); await flushPromises()
    await wrapper.setProps({ packageId: 'other' }); await flushPromises()
    expect(wrapper.get('[data-testid=sample-create]').attributes('disabled')).toBeDefined()
    expect(wrapper.find('[data-testid=sample-boundaries]').exists()).toBe(false)
    wrapper.unmount()
  })
})
