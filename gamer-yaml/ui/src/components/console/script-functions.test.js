import { describe, it, expect } from 'vitest'
import { load } from 'js-yaml'
import { addScriptFunction, scriptCallSnippet } from './script-functions'
describe('script functions source editing', () => {
  it('adds functions without rewriting comments or visual steps', () => {
    const source = '# keep\ntargets: {done: {template: done.png}}\nrun:\n  - finish: done # keep too\n'
    const edited = addScriptFunction(source, '领取')
    expect(edited.startsWith(source)).toBe(true)
    expect(load(edited).functions.领取.run).toEqual([{ log: '领取' }])
  })
  it('extends a function section and preserves all other source', () => {
    const source = 'functions: # comment\n  old:\n    run: [{return: true}] # keep\nrun: [{old: {}}]\n'
    const edited = addScriptFunction(source, 'helper')
    expect(edited).toContain('run: [{return: true}] # keep')
    expect(edited).toContain('run: [{old: {}}]')
    expect(Object.keys(load(edited).functions)).toEqual(['old', 'helper'])
    expect(() => addScriptFunction(edited, 'helper')).toThrow('同名')
  })
  it('refuses implicit rewriting and creates a qualified call', () => {
    expect(() => addScriptFunction('functions: {old: {run: []}}\nrun: []\n', 'helper')).toThrow('分行缩进')
    expect(() => addScriptFunction('run: []\n', '../bad')).toThrow('函数名')
    expect(load(`run:\n${scriptCallSnippet('common.yaml', '领取')}`).run).toEqual([{ 'common.yaml#领取': {} }])
  })
})
