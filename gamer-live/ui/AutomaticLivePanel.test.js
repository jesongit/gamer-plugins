import { afterEach, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import AutomaticLivePanel from './AutomaticLivePanel.vue'

let wrapper
const output = () => ({ device_id: 'different-device', mode: 'local', push_url: 'old-secret', audio: true, fps: 30, bitrate_kbps: 4000 })
const target = { device_id: 'phone', package_id: 'daily' }
const open = (props = {}) => {
  wrapper = mount(AutomaticLivePanel, { props: {
    status: { state: 'stopped' }, queue: { target, waiting: [] },
    devices: [{ id: 'phone', name: '直播手机' }], output: output(), perform: vi.fn(async () => ({})), ...props,
  } })
  return wrapper
}
const button = text => wrapper.findAll('button').find(item => item.text() === text)
afterEach(() => wrapper?.unmount())

it('查看目录只请求文件列表，修改目录后丢弃旧列表', async () => {
  const perform = vi.fn(async () => ({ folder: 'D:\\clips', files: [{ name: '01.mp4' }, { name: '02.webm' }] }))
  open({ perform })
  await wrapper.get('[aria-label="视频文件夹"]').setValue(' D:\\clips ')
  await button('查看视频列表').trigger('click'); await flushPromises()
  expect(perform).toHaveBeenCalledExactlyOnceWith('automatic.preview', { video_folder: 'D:\\clips' })
  expect(wrapper.findAll('ol li').map(item => item.text())).toEqual(['01.mp4', '02.webm'])
  expect(wrapper.text()).toContain('按文件名轮播，不含子目录')
  await wrapper.get('[aria-label="视频文件夹"]').setValue('D:\\other')
  expect(wrapper.find('ol').exists()).toBe(false)
})

it('启动使用互动队列绑定目标，提交输出设置并清除推流凭据', async () => {
  const perform = vi.fn(async () => ({ state: 'video' })), options = output()
  open({ perform, output: options })
  await wrapper.get('[aria-label="视频文件夹"]').setValue('D:\\clips')
  await wrapper.get('form').trigger('submit'); await flushPromises()
  expect(perform).toHaveBeenCalledExactlyOnceWith('automatic.start', {
    video_folder: 'D:\\clips', device_id: 'phone', package_id: 'daily', mode: 'local', push_url: '', audio: true, fps: 30, bitrate_kbps: 4000,
  })
  expect(options.push_url).toBe('')
})

it('没有执行目标时解释下一步，无法启动', async () => {
  const perform = vi.fn()
  open({ perform, queue: { waiting: [] } })
  await wrapper.get('[aria-label="视频文件夹"]').setValue('D:\\clips')
  expect(button('启动自动直播').element.disabled).toBe(true)
  await wrapper.get('form').trigger('submit')
  expect(perform).not.toHaveBeenCalled()
  await button('设置执行目标').trigger('click')
  expect(wrapper.emitted('configure-target')).toHaveLength(1)
})

it('页面重新打开恢复服务端目录和执行状态，展示真实队列与视频', async () => {
  open({ status: { state: 'executing', video_folder: 'D:\\saved', stream: { source_kind: 'device', video_file: 'D:\\saved\\01.mp4', video_position_secs: 65 }, queue: { target, current: { name: '云崩铁日常' }, waiting: [{ id: 'q2' }, { id: 'q3' }] } } })
  expect(wrapper.get('[aria-label="视频文件夹"]').element.value).toBe('D:\\saved')
  expect(wrapper.text()).toContain('执行目标实时画面')
  expect(wrapper.text()).toContain('云崩铁日常')
  expect(wrapper.text()).toContain('2 项')
  expect(wrapper.text()).toContain('01.mp4')
  expect(wrapper.text()).toContain('1:05')
  expect(wrapper.get('fieldset').element.disabled).toBe(true)
  await button('查看触发日志').trigger('click')
  expect(wrapper.emitted('open-logs')).toHaveLength(1)
})

it('停止中锁定配置和重复停止，停止仅调用专用模式接口', async () => {
  const perform = vi.fn(async () => ({ state: 'stopping' }))
  open({ perform, status: { state: 'video', video_folder: 'D:\\clips' } })
  await button('停止自动直播').trigger('click'); await flushPromises()
  expect(perform).toHaveBeenCalledExactlyOnceWith('automatic.stop')
  await wrapper.setProps({ status: { state: 'stopping', video_folder: 'D:\\clips' } })
  expect(button('正在停止…').element.disabled).toBe(true)
  expect(wrapper.get('fieldset').element.disabled).toBe(true)
})

it('目录为空与输出失败如实显示，失败后仍可修正目录', async () => {
  open({ perform: vi.fn(async () => ({ folder: 'D:\\empty', files: [] })) })
  await wrapper.get('[aria-label="视频文件夹"]').setValue('D:\\empty')
  await button('查看视频列表').trigger('click'); await flushPromises()
  expect(wrapper.text()).toContain('没有可播放的视频')
  await wrapper.setProps({ status: { state: 'failed', error: '视频无法解码', video_folder: 'D:\\empty' } })
  expect(wrapper.get('[role="alert"]').text()).toContain('视频无法解码')
  expect(wrapper.get('fieldset').element.disabled).toBe(false)
})

it('暂停时根据实际输出源显示画面，等待项保持可见', async () => {
  open({ status: { state: 'paused', stream: { source_kind: 'device' }, queue: { target, waiting: [{ id: 'q1' }] }, error: '恢复视频失败' } })
  expect(wrapper.text()).toContain('执行目标实时画面')
  expect(wrapper.text()).toContain('1 项')
  expect(wrapper.text()).toContain('触发日志')
  expect(wrapper.get('[role="alert"]').text()).toContain('恢复视频失败')
})

it('停止状态重新打开仍显示保存目录和队列目标，不自动启动', async () => {
  const perform = vi.fn()
  open({ perform, status: { state: 'stopped', video_folder: 'D:/code/gamer/live-videos', queue: { target, waiting: [] } } })
  expect(wrapper.get('[aria-label="视频文件夹"]').element.value).toBe('D:/code/gamer/live-videos')
  expect(wrapper.text()).toContain('直播手机 · daily')
  expect(wrapper.get('fieldset').element.disabled).toBe(false)
  expect(button('启动自动直播').element.disabled).toBe(false)
  expect(perform).not.toHaveBeenCalled()
})
