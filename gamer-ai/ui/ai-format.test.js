import { expect, it } from 'vitest'
import { eventDetails, safeImage, usageValue } from './ai-format'
it('token usage缺失和null不会误报零', () => {
  expect(usageValue({ total_tokens: null }, ['total_tokens'])).toBe('未知')
  expect(usageValue({}, ['total_tokens'])).toBe('未知')
  expect(usageValue({ total_tokens: 0 }, ['total_tokens'])).toBe('0')
})
it('图片仅接受inline标准图片数据，不接受远程URL或脚本', () => {
  expect(safeImage('https://example.com/game.png')).toBe('')
  expect(safeImage('data:text/html;base64,AA==')).toBe('')
  expect(safeImage('data:image/png;base64,AA==')).toBe('data:image/png;base64,AA==')
})
it('详情对嵌套secret/token作脱敏，保留无敏感操作数据', () => {
  const value = eventDetails({ x: 42, result: { token: 'a', api_key: 'b', y: 7 }, content: [{ image_data_url: 'c', text: '完成' }] })
  expect(value).toContain('42'); expect(value).toContain('完成'); expect(value).toContain('7')
  expect(value).not.toContain('api_key'); expect(value).not.toContain('token'); expect(value).not.toContain('image_data_url')
})
