import { templateShortName } from '../console/template-resource'
import { parseFunctionLibrary, parseScript, serialize } from './codec'
import { walkSteps, type Cell } from './model'
import type { EditorModel } from './commands'

export interface TemplateRename {
  oldName: string
  newName: string
}

// 与 host/syntax.rs 的模板引用改写一致：只改视觉参数和模板分支，
// 不改日志、普通字符串、变量引用或用户函数的非模板实参。
export function renameTemplateReferences(model: EditorModel, { oldName, newName }: TemplateRename): void {
  const oldShort = templateShortName(oldName)
  const newShort = templateShortName(newName)
  const renamed = (value: unknown) => typeof value === 'string'
    && (value === oldShort || value === oldName || value.split(/[\\/]/).at(-1) === oldName)
    ? newShort : value
  const renameCell = (cell?: Cell) => {
    if (cell && 'lit' in cell) cell.lit = renamed(cell.lit)
  }
  const runs = 'functions' in model ? model.functions.map(fn => fn.run) : [model.run]
  for (const run of runs) walkSteps(run, step => {
    if (step.kind === 'match_templates') {
      for (const branch of step.cases) renameCell(branch.template)
    } else if (step.kind === 'call') {
      if (['find', 'wait_find', 'tap_template', 'wait_disappear'].includes(step.fn)) {
        renameCell(step.args.kind === 'value' ? step.args.cell
          : step.args.kind === 'map' ? step.args.entries.template : undefined)
      }
      if (step.args.kind === 'map' && ['wait_find', 'find_any'].includes(step.fn)) {
        const cell = step.args.entries[step.fn === 'find_any' ? 'templates' : 'obstacles']
        if (cell && Array.isArray(cell.lit)) cell.lit = cell.lit.map(renamed)
      }
    }
  })
}

export function parseEditorSource(kind: string, source: string) {
  return kind === 'script' ? parseScript(source) : parseFunctionLibrary(source)
}

export function renamedTemplateSource(kind: string, source: string, rename: TemplateRename): string | null {
  const parsed = parseEditorSource(kind, source)
  if (parsed.diagnostics.length) return null
  renameTemplateReferences(parsed.model, rename)
  return serialize(parsed.model)
}
