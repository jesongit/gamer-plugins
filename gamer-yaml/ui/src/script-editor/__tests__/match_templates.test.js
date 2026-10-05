// @vitest-environment happy-dom
import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import { nextTick } from 'vue'
import { parseScript, serialize } from '../codec'
import { validateSource } from '../validation'
import { childStepLists, cloneStepWithNewUuids } from '../model'
import { resolveStepList } from '../commands'
import { findStepLocation } from '../selection'
import { locateDiagnostic } from '../components/kinds'
import StepCard from '../components/StepCard.vue'
import { createControl } from '../factories'
import { setupScript } from './component_helpers'

const source = `run:
  - match_templates:
      cases:
        - template: notice.png
          as: hit
          do:
            - tap: $hit.center
        - template: login.png
          do:
            - log: login
      else:
        - log: missing
`

describe('模板分支：完整编辑与执行路径契约', () => {
  it('旧脚本和新建步骤默认单次匹配，保存重开保留次数与间隔', async () => {
    const { model, stack } = setupScript(source)
    const step = model.run[0]
    expect(step.times).toEqual({ lit: 1 })
    expect(step.interval).toEqual({ lit: '250ms' })
    expect(createControl('match_templates')).toMatchObject({ times: { lit: 1 }, interval: { lit: '250ms' } })
    const wrapper = mount(StepCard, { props: { model, stack, step, index: 0, containerPath: ['run'], basePath: 'run', expandedUuids: new Set([step.uuid]) } })
    await wrapper.get('input[aria-label="匹配次数"]').setValue('999')
    expect(step.times.lit).toBe(999)
    stack.undo()
    expect(step.times.lit).toBe(1)
    stack.redo()
    await wrapper.get('input[aria-label="匹配间隔数值"]').setValue('1')
    await wrapper.get('select[aria-label="匹配间隔单位"]').setValue('s')
    expect(step.interval.lit).toBe('1s')
    stack.undo()
    expect(step.interval.lit).toBe('1ms')
    stack.redo()
    const reopened = parseScript(serialize(model)).model.run[0]
    expect(reopened).toMatchObject({ times: { lit: 999 }, interval: { lit: '1s' } })
    expect(validateSource(serialize(model), 'script').diagnostics).toEqual([])
    wrapper.unmount()
  })

  it('多轮匹配允许退出匹配循环，默认或显式一次不新建循环作用域', () => {
    const branch = 'cases: [{template: a.png, do: [{break: {}}]}]'
    expect(validateSource(`run: [{match_templates: {times: 999, ${branch}}}]`, 'script').diagnostics).toEqual([])
    for (const times of ['', 'times: 1, ']) {
      expect(validateSource(`run: [{match_templates: {${times}${branch}}}]`, 'script').diagnostics[0].code).toBe('yaml.break.outside_loop')
      expect(validateSource(`run: [{repeat: 3, do: [{match_templates: {${times}${branch}}}]}]`, 'script').diagnostics).toEqual([])
    }
  })

  it('次数和间隔支持参数引用并校验引用类型', () => {
    const text = `params:\n  rounds: {type: integer, default: 3}\n  gap: {type: duration, default: 1s}\nrun:\n  - match_templates:\n      times: $rounds\n      interval: $gap\n      cases: [{template: a.png, do: []}]\n`
    expect(validateSource(text, 'script').diagnostics).toEqual([])
    expect(validateSource(text.replace('rounds: {type: integer, default: 3}', 'rounds: {type: boolean, default: true}'), 'script').diagnostics.some(d => d.field === 'times')).toBe(true)
    expect(validateSource(text.replace('gap: {type: duration, default: 1s}', 'gap: {type: boolean, default: true}'), 'script').diagnostics.some(d => d.field === 'interval')).toBe(true)
  })

  it.each([0, -1, 1.5, '"3"', 'null', 'true'])('拒绝无效匹配次数 %s', times => {
    expect(validateSource(`run: [{match_templates: {times: ${times}, cases: [{template: a.png, do: []}]}}]`, 'script').diagnostics.some(d => d.code === 'yaml.match_templates.times')).toBe(true)
  })

  it.each(['-1ms', '-1', 'wrong', 'null', 'true'])('拒绝无效匹配间隔 %s', interval => {
    expect(validateSource(`run: [{match_templates: {interval: ${interval}, cases: [{template: a.png, do: []}]}}]`, 'script').diagnostics.some(d => d.code === 'yaml.match_templates.interval')).toBe(true)
  })
  it('保存重开、分支局部变量、嵌套定位和复制', () => {
    const first = validateSource(source, 'script', { knownFunctions: new Set(['tap', 'log']), resolveTemplate: () => true })
    expect(first.diagnostics).toEqual([])
    const model = first.result.model
    const text = serialize(model)
    expect(validateSource(text, 'script').diagnostics).toEqual([])
    expect(serialize(parseScript(text).model)).toBe(text)
    const child = model.run[0].cases[0].body[0]
    const location = findStepLocation(model, child.uuid)
    expect(location.stepPath).toBe('run[0].cases[0].do[0]')
    expect(resolveStepList(model, location.containerPath)[0]).toBe(child)
    expect(locateDiagnostic(model, { step_path: location.stepPath }).uuid).toBe(child.uuid)
    const clone = cloneStepWithNewUuids(model.run[0])
    expect(childStepLists(clone)).toHaveLength(3)
    expect(clone.cases[0].body[0].uuid).not.toBe(child.uuid)
    expect(validateSource(source + '  - tap: $hit.center\n', 'script').diagnostics.some(d => d.message.includes('hit'))).toBe(true)
    expect(validateSource(source.replace('log: login', 'tap: $hit.center'), 'script').diagnostics.some(d => d.message.includes('hit'))).toBe(true)
  })

  it('模板选择、分支新增与排序、子步骤撤销重做保持对象关联', async () => {
    const { model, stack } = setupScript(source)
    const step = model.run[0]
    const wrapper = mount(StepCard, { props: {
      model, stack, step, index: 0, containerPath: ['run'], basePath: 'run',
      expandedUuids: new Set([step.uuid]), templates: ['notice.png', 'login.png', 'home.png'],
    } })
    expect(wrapper.findAll('.template-case')).toHaveLength(2)
    expect(wrapper.findAll('.template-case .tpl-toggle')).toHaveLength(2)
    await wrapper.get('input[aria-label="分支 2 模板"]').setValue('home.png')
    const child = step.cases[0].body[0]
    stack.apply({ type: 'update_step', path: ['run', 0, 'cases[0].do', 0], fields: { fn: 'log' } })
    await wrapper.get('button[aria-label="下移模板分支 1"]').trigger('click')
    expect(step.cases[1].body[0]).toBe(child)
    stack.undo()
    stack.undo()
    expect(step.cases[0].body[0].fn).toBe('tap')
    stack.redo()
    stack.redo()
    expect(step.cases[1].body[0].fn).toBe('log')
    await nextTick()
    await wrapper.get('button[aria-label="添加模板分支"]').trigger('click')
    expect(step.cases).toHaveLength(3)
    await wrapper.get('input[aria-label="分支 3 模板"]').setValue('login.png')
    await wrapper.get('input[aria-label="分支 3 匹配结果"]').setValue('login_hit')
    const reopened = parseScript(serialize(model)).model
    expect(reopened.run[0].cases[2]).toMatchObject({ template: { lit: 'login.png' }, as: 'login_hit', body: [] })
    await wrapper.get('button[aria-label="删除模板分支 3"]').trigger('click')
    expect(step.cases).toHaveLength(2)
    stack.undo()
    expect(step.cases[2].as).toBe('login_hit')
  })

  it.each([
    'cases: []',
    'cases: [{template: "", do: []}]',
    'cases: [{template: a, do: [], unknown: 1}]',
    'cases: [{template: a}]',
    'cases: [{template: a, as: 123, do: []}]',
    'cases: [{template: a, do: []}]\n      threshold: 2',
  ])('拒绝无效分支 %s', body => {
    expect(validateSource(`run:\n  - match_templates:\n      ${body}\n`, 'script').diagnostics.length).toBeGreaterThan(0)
  })
})
