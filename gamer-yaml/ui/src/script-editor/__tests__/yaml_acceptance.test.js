// @vitest-environment happy-dom
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it, vi } from 'vitest'
import { load as loadYaml } from 'js-yaml'
import { useSourceYamlEditor } from '../../composables/useSourceYamlEditor'
import { mount } from '@vue/test-utils'
import { serialize } from '../codec'
import { hasParamDefault } from '../schema'
import { childStepLists, PARAM_TYPES } from '../model'
import { validateSource } from '../validation'
import { SE_TARGET_OPTIONS } from '../targets'
import StepCard from '../components/StepCard.vue'
import { setupScript, expandCard } from './component_helpers'

const fixture = file => readFileSync(new URL('../../../../../../tools/yaml-tests/' + file, import.meta.url), 'utf8')
const catalog = JSON.parse(fixture('native-functions.json'))
const ctx = {
  knownFunctions: new Set([...catalog.map(f => f.name), 'echo_value', 'nested_echo', 'early_return']),
  resolveParams: name => catalog.find(f => f.name === name)?.params,
  resolveTemplate: name => name === 'button.png',
}
const provide = { [SE_TARGET_OPTIONS]: {
  targets: catalog.map(f => ({ target: f.name, group: 'plugin', label: f.name })),
  resolveParamsSync: ctx.resolveParams,
  resolveParams: async name => ctx.resolveParams(name),
} }

describe('YAML v2 公开源码与编辑边界', () => {
  for (const file of ['_function.yaml', 'flow.yaml', 'native.yaml']) {
    it(`${file}：完整源码经过原文编辑器逐字保存，语义由服务端权威校验`, async () => {
      const source = fixture(file)
      const doc = loadYaml(source)
      expect(doc.version).toBe(2)
      const isFunction = file.startsWith('_function')
      const get = vi.fn().mockResolvedValue({ content: source, version: 'v1' })
      const update = vi.fn().mockResolvedValue({ version: 'v2' })
      const editor = useSourceYamlEditor({ api: { getScript: get, getFunction: get, updateScript: update, updateFunction: update }, call: vi.fn() })
      editor.reset('fixture', isFunction ? 'function_library' : 'script')
      await editor.load(`fixture/${file}`)
      await editor.save()
      expect(update).toHaveBeenCalledWith(`fixture/${file}`, { content: source, expected_version: 'v1' })
      expect(editor.content.value).toBe(source)
    })
  }

  function tutorialExamples() {
    const tutorial = readFileSync(resolve(__dirname, '../../../../../../docs/guides/yaml-tutorial.md'), 'utf8')
    const examples = [...tutorial.matchAll(/```yaml\r?\n([\s\S]*?)```/g)]
    expect(examples.length).toBeGreaterThan(0)
    return examples.map(([, source]) => ({ source, document: loadYaml(source) }))
  }

  it('教程完整文档都显式声明 v2，脚本含视觉完成条件，函数库有定义', () => {
    for (const { document } of tutorialExamples()) {
      expect(document.version).toBe(2)
      if (document.functions) expect(Object.keys(document.functions).length).toBeGreaterThan(0)
      else {
        expect(Array.isArray(document.run)).toBe(true)
        expect(document.run.some(step => Object.hasOwn(step, 'finish'))).toBe(true)
        expect(Object.keys(document.targets).length).toBeGreaterThan(0)
      }
    }
  })

  it('教程区分只观察、可选弹窗与完成条件，不让旧表单重序列化新语法', () => {
    const document = tutorialExamples()[0].document
    expect(document.run[0]).toMatchObject({ wait: 'claim', as: 'button', then: [{ tap: '$button' }] })
    expect(document.run[1].optional).toMatchObject({ find: 'confirm', timeout: '0ms' })
    expect(document.run.at(-1)).toMatchObject({ finish: 'done' })
    expect(validateSource(tutorialExamples()[0].source, 'script', ctx).diagnostics.length).toBeGreaterThan(0)
  })

  it('教程列出全部参数类型', () => {
    const tutorial = readFileSync(resolve(__dirname, '../../../../../../docs/guides/yaml-tutorial.md'), 'utf8')
    for (const type of PARAM_TYPES) expect(tutorial).toContain(`| \`${type}\` |`)
  })

  it('文本、时间、返回字面量不误报模板缺失，模板参数仍校验', () => {
    const source = 'run:\n  - log: hello\n  - sleep: 1s\n  - if: null\n    then: []\n  - return: done\n'
    expect(validateSource(source, 'script', { ...ctx, resolveTemplate: () => false }).diagnostics).toEqual([])
    expect(validateSource('run:\n  - wait_find: missing\n', 'script', ctx).diagnostics.map(d => d.code))
      .toEqual(['yaml.resource.tmpl_not_found'])
    expect(validateSource('run:\n  - return: {items: [$missing, $$literal]}\n', 'script', ctx).diagnostics.map(d => d.code))
      .toEqual(['yaml.var.undefined'])
    expect(validateSource('run:\n  - custom: hello\n', 'script', {
      resolveTemplate: () => false,
      resolveParams: () => [{ name: 'text', type: 'string' }, { name: 'template', type: 'template' }],
    }).diagnostics).toEqual([])
  })
})

describe(`全部 ${catalog.length} 个内置函数的真实参数 Schema 表单`, () => {
  it('障碍模板列表可选取、保存重开和删除，非法元素与缺失模板报错', async () => {
    const created = setupScript('run:\n  - wait_find: button.png\n')
    const wrapper = mount(StepCard, { props: { ...created, step: created.model.run[0], containerPath: ['run'], basePath: 'run', index: 0, templates: ['button.png'] }, global: { provide } })
    await expandCard(wrapper, created.model.run[0].uuid)
    await wrapper.get('button[data-param="obstacles"]').trigger('click')
    await wrapper.get('button[aria-label="添加参数 obstacles"]').trigger('click')
    await wrapper.get('input[aria-label="参数 obstacles 1"]').setValue('button.png')
    const reopened = setupScript(serialize(created.model))
    expect(reopened.model.run[0].args.entries.obstacles).toEqual({lit: ['button.png']})
    expect(validateSource(serialize(created.model), 'script', ctx).diagnostics).toEqual([])
    await wrapper.get('button[aria-label="删除参数 obstacles 1"]').trigger('click')
    expect(created.model.run[0].args.entries.obstacles).toEqual({lit: []})
    wrapper.unmount()
    for (const value of ['[1]', '[""]', 'bad']) {
      expect(validateSource(`run:\n  - wait_find: {template: button.png, obstacles: ${value}}\n`, 'script', ctx).diagnostics.map(d => d.code)).toContain('yaml.args.type')
    }
    expect(validateSource('run:\n  - wait_find: {template: button.png, obstacles: [missing.png]}\n', 'script', ctx).diagnostics.map(d => d.code)).toContain('yaml.resource.tmpl_not_found')
  })

  for (const fn of catalog) {
    it(`${fn.name}：参数名称是纯文本，添加只能使用声明的参数`, async () => {
      const created = setupScript(`run:\n  - ${fn.name}: {}\n`)
      const wrapper = mount(StepCard, { props: { ...created, step: created.model.run[0], containerPath: ['run'], basePath: 'run', index: 0 }, global: { provide } })
      await expandCard(wrapper, created.model.run[0].uuid)
      expect(wrapper.findAll('.param-button')).toHaveLength(fn.params.filter(p => !p.required).length)
      expect(wrapper.findAll('.arg-name').map(w => w.text())).toEqual(fn.params.filter(p => p.required).map(p => p.name))
      for (const param of fn.params.filter(p => !p.required)) {
        const button = wrapper.get(`button[data-param="${param.name}"]`)
        expect(button.attributes('aria-pressed')).toBe(String(param.required && !hasParamDefault(param)))
        if (!param.required || hasParamDefault(param)) await button.trigger('click')
      }
      expect(wrapper.findAll('.arg-name').map(w => w.text()).sort()).toEqual(fn.params.map(p => p.name).sort())
      expect(wrapper.findAll('input[aria-label^="参数名"]')).toHaveLength(0)
      expect(wrapper.findAll('input[readonly]')).toHaveLength(0)
      expect(Object.keys(created.model.run[0].args.entries || {}).sort()).toEqual(fn.params.filter(p => !p.required && hasParamDefault(p)).map(p => p.name).sort())
      wrapper.unmount()
    })
  }

  it('wait_find 默认仅观察，显式点击开关的真与假均可保存重开', async () => {
    const created = setupScript('run:\n  - wait_find: button.png\n')
    const wrapper = mount(StepCard, { props: { ...created, step: created.model.run[0], containerPath: ['run'], basePath: 'run', index: 0 }, global: { provide } })
    await expandCard(wrapper, created.model.run[0].uuid)
    const more = wrapper.get('details.optional-params')
    more.element.open = true
    const button = more.get('button[data-param="click"]')
    expect(button.text()).toContain('false')
    expect(button.attributes('aria-pressed')).toBe('false')
    await button.trigger('click')
    const input = more.get('select[aria-label="参数 click"]')
    expect(input.element.value).toBe('false')
    await input.setValue('true')
    const clicking = setupScript(serialize(created.model))
    expect(clicking.model.run[0].args.entries.click).toEqual({ lit: true })
    expect(validateSource(serialize(created.model), 'script', ctx).diagnostics).toEqual([])
    await input.setValue('false')
    const reopened = setupScript(serialize(created.model))
    expect(reopened.model.run[0].args.entries.click).toEqual({ lit: false })
    expect(validateSource(serialize(created.model), 'script', ctx).diagnostics).toEqual([])
    wrapper.unmount()
  })

  it('位置简写使用参数类型控件，补默认参数保留用户输入', async () => {
    const created = setupScript('run:\n  - wait_find: button\n')
    const wrapper = mount(StepCard, { props: { ...created, step: created.model.run[0], containerPath: ['run'], basePath: 'run', index: 0 }, global: { provide } })
    await expandCard(wrapper, created.model.run[0].uuid)
    await wrapper.get('button[data-param="timeout"]').trigger('click')
    expect(created.model.run[0].args.entries.template).toEqual({ lit: 'button' })
    await wrapper.get('input[aria-label="参数 timeout数值"]').setValue('5')
    expect(created.model.run[0].args.entries.timeout).toEqual({ lit: '5s' })
    wrapper.unmount()
  })
})
