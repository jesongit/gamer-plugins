import { describe, expect, it } from 'vitest'
import fixtures from '../../../../tests/reference-types.json'
import { validateSource } from '../validation'

const params = {
  tap: [{ name: 'position', type: 'point' }],
  tap_template: [{ name: 'template', type: 'template' }],
  find: [{ name: 'template', type: 'template' }],
  find_any: [{ name: 'templates', type: 'list', items: { type: 'template' } }],
  log: [{ name: 'message', type: 'any' }],
}
describe('保存前引用类型：与服务端共用样例', () => {
  it.each(fixtures)('$name', fixture => {
    const { diagnostics } = validateSource(fixture.source, fixture.kind || 'script', {
      resolveParams: name => params[name] || null,
    })
    const errors = diagnostics.filter(d => d.code === 'yaml.args.ref_type')
    expect(errors.map(d => `${d.step_path}.${d.field}`)).toEqual(fixture.errors)
  })
})
