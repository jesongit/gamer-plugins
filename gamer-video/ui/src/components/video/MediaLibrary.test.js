// @vitest-environment happy-dom
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'

vi.mock('./videoApi', () => ({ videoApi: {
  recordingHistory: vi.fn(async () => []),
  activeRecording: vi.fn(async () => null),
  renameMedia: vi.fn(),
} }))
import MediaLibrary from './MediaLibrary.vue'
import { videoApi } from './videoApi'

const media = { id: 'clip', name: '旧视频.mp4', state: 'ready', source: 'import', refs: [{ package_id: 'pkg', plugin_id: 'gamer-video', kind: 'project' }] }
beforeEach(() => vi.clearAllMocks())

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
