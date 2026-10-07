// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'

vi.mock('./videoApi', () => ({ videoApi: {
  recordingHistory: vi.fn(async () => []),
  activeRecording: vi.fn(async () => null),
  recordingStop: vi.fn(),
  recordingCancel: vi.fn(),
  recordingStatus: vi.fn(),
  recordingEvents: vi.fn(async () => []),
  renameMedia: vi.fn(),
} }))
import MediaLibrary from './MediaLibrary.vue'
import { videoApi } from './videoApi'
import { devicesData } from '../../../../../../web/src/store'

const media = { id: 'clip', name: '旧视频.mp4', state: 'ready', source: 'import', refs: [{ package_id: 'pkg', plugin_id: 'gamer-video', kind: 'project' }] }
beforeEach(() => { vi.clearAllMocks(); devicesData.value = [] })
afterEach(() => vi.useRealTimers())

const recording = { id: 'record-1', device_id: 'browser-test', state: 'recording', segments: [], event_count: 0 }
const finalizing = { ...recording, state: 'finalizing' }
async function recordingPanel() {
  devicesData.value = [{ id: recording.device_id, name: '浏览器', capabilities: { recording: true } }]
  videoApi.activeRecording.mockResolvedValue(recording)
  const wrapper = mount(MediaLibrary)
  await wrapper.get('[data-testid="record-device"]').setValue(recording.device_id)
  await flushPromises()
  return wrapper
}

describe('录制后台保存', () => {
  it('接受停止后继续显示保存中，实际完成才刷新素材和通知工作台', async () => {
    vi.useFakeTimers()
    const wrapper = await recordingPanel()
    videoApi.recordingStop.mockResolvedValue(finalizing)
    videoApi.recordingStatus.mockResolvedValue(finalizing)
    await wrapper.get('[data-testid="record-stop"]').trigger('click')
    await flushPromises()
    expect(wrapper.get('[data-testid="record-saving"]').text()).toContain('录制已停止')
    expect(wrapper.get('[data-testid="record-stop"]').element.disabled).toBe(true)
    expect(wrapper.emitted('recording-finished')).toBeUndefined()
    expect(wrapper.emitted('changed')).toBeUndefined()
    const completed = { ...recording, state: 'completed' }
    videoApi.recordingStatus.mockResolvedValue(completed)
    videoApi.activeRecording.mockResolvedValue(null)
    await vi.advanceTimersByTimeAsync(3000)
    await flushPromises()
    expect(wrapper.find('[data-testid="record-saving"]').exists()).toBe(false)
    expect(wrapper.emitted('recording-finished')).toEqual([[completed]])
    expect(wrapper.emitted('changed')).toHaveLength(1)
    await vi.advanceTimersByTimeAsync(3000)
    expect(wrapper.emitted('recording-finished')).toHaveLength(1)
    wrapper.unmount()
  })

  it('转码失败展示原因，不发布保存成功事件', async () => {
    vi.useFakeTimers()
    const wrapper = await recordingPanel()
    videoApi.recordingStop.mockResolvedValue(finalizing)
    videoApi.recordingStatus.mockResolvedValue(finalizing)
    await wrapper.get('[data-testid="record-stop"]').trigger('click')
    await flushPromises()
    videoApi.recordingStatus.mockResolvedValue({ ...recording, state: 'failed', error: '编码失败，原始帧已保留' })
    await vi.advanceTimersByTimeAsync(3000)
    await flushPromises()
    expect(wrapper.get('[data-testid="media-error"]').text()).toContain('原始帧已保留')
    expect(wrapper.emitted('recording-finished')).toBeUndefined()
    expect(wrapper.emitted('changed')).toHaveLength(1)
    wrapper.unmount()
  })

  it('切换设备后忽略旧录制迟到的完成响应', async () => {
    const wrapper = await recordingPanel()
    videoApi.recordingStop.mockResolvedValue(finalizing)
    let resolveStatus
    videoApi.recordingStatus.mockImplementation(() => new Promise(resolve => { resolveStatus = resolve }))
    await wrapper.get('[data-testid="record-stop"]').trigger('click')
    await flushPromises()
    devicesData.value.push({ id: 'browser-other', name: '另一个浏览器' })
    videoApi.activeRecording.mockResolvedValue(null)
    await wrapper.get('[data-testid="record-device"]').setValue('browser-other')
    resolveStatus({ ...recording, state: 'completed' })
    await flushPromises()
    expect(wrapper.emitted('recording-finished')).toBeUndefined()
    wrapper.unmount()
  })
})

describe('素材库视频重命名', () => {
  it('删除后显示入口，编辑不会切换预览，保存成功才刷新列表', async () => {
    let finish
    videoApi.renameMedia.mockImplementationOnce(() => new Promise(resolve => { finish = resolve }))
    const wrapper = mount(MediaLibrary, { attachTo: document.body, props: { mediaList: [media] } })
    const row = wrapper.get('[data-testid="media-row"]')
    expect(row.findAll('.row-actions button').map(button => button.text())).toEqual(['删除', '重命名'])
    await row.get('[data-testid="media-rename"]').trigger('click')
    const input = wrapper.get('[data-testid="media-rename-input"]')
    expect(input.element).toBe(document.activeElement)
    expect(input.element.value).toBe(media.name)
    await input.setValue('  新视频.mp4  ')
    await wrapper.get('[data-testid="media-rename-form"]').trigger('submit')
    expect(videoApi.renameMedia).toHaveBeenCalledWith('clip', '新视频.mp4')
    expect(wrapper.emitted('select')).toBeUndefined()
    expect(wrapper.emitted('changed')).toBeUndefined()
    expect(wrapper.get('[data-testid="media-rename-save"]').element.disabled).toBe(true)
    finish({ ...media, name: '新视频.mp4' })
    await flushPromises()
    expect(wrapper.emitted('changed')).toHaveLength(1)
    expect(wrapper.find('[data-testid="media-rename-form"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('空名称不能保存，失败保留输入供重试，Esc 取消不会写入', async () => {
    videoApi.renameMedia.mockRejectedValueOnce(new Error('网络不可用'))
    const wrapper = mount(MediaLibrary, { props: { mediaList: [media] } })
    await wrapper.get('[data-testid="media-rename"]').trigger('click')
    const input = wrapper.get('[data-testid="media-rename-input"]')
    await input.setValue('   ')
    await wrapper.get('[data-testid="media-rename-form"]').trigger('submit')
    expect(videoApi.renameMedia).not.toHaveBeenCalled()
    await input.setValue('新名称')
    await wrapper.get('[data-testid="media-rename-form"]').trigger('submit')
    await flushPromises()
    expect(wrapper.get('[data-testid="media-error"]').text()).toContain('网络不可用')
    expect(input.element.value).toBe('新名称')
    expect(wrapper.emitted('changed')).toBeUndefined()
    await input.trigger('keydown', { key: 'Escape' })
    expect(wrapper.find('[data-testid="media-rename-form"]').exists()).toBe(false)
    expect(videoApi.renameMedia).toHaveBeenCalledTimes(1)
    wrapper.unmount()
  })
})
