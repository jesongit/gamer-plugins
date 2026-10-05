import { templateShortName } from '../console/template-resource'
import { parseFunctionLibrary, parseScript, serialize } from './codec'
import { childStepLists, walkSteps, type Cell, type Step } from './model'
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

function templateSlots(model: EditorModel): Map<string, string> {
  const slots = new Map<string, string>()
  const add = (path: string, cell?: Cell) => {
    if (cell && typeof cell.lit === 'string') slots.set(path, cell.lit)
  }
  const visit = (run: Step[], path: string) => run.forEach((step, i) => {
    const key = `${path}[${i}]`
    if (step.kind === 'match_templates') step.cases.forEach((branch, n) => add(`${key}.cases[${n}]`, branch.template))
    if (step.kind === 'call') {
      if (['find', 'wait_find', 'tap_template', 'wait_disappear'].includes(step.fn)) {
        add(`${key}.template`, step.args.kind === 'value' ? step.args.cell
          : step.args.kind === 'map' ? step.args.entries.template : undefined)
      }
      if (step.args.kind === 'map' && ['wait_find', 'find_any'].includes(step.fn)) {
        const cell = step.args.entries[step.fn === 'find_any' ? 'templates' : 'obstacles']
        if (cell && Array.isArray(cell.lit)) cell.lit.forEach((value, n) => add(`${key}.list[${n}]`, { lit: value }))
      }
    }
    for (const child of childStepLists(step)) visit(child.list, `${key}.${child.key}`)
  })
  if ('functions' in model) model.functions.forEach(fn => visit(fn.run, `functions.${fn.name}.run`))
  else visit(model.run, 'run')
  return slots
}

// 另一页面可能已重命名，而本页没收到通知。只接受“旧模板已不存在、
// 新模板确实存在、磁盘全部变化仅是模板引用改写”的可验证变化。
export function inferTemplateRenames(kind: string, before: string, after: string,
  resolveTemplate: (name: string) => boolean): TemplateRename[] {
  const baseline = parseEditorSource(kind, before), latest = parseEditorSource(kind, after)
  if (baseline.diagnostics.length || latest.diagnostics.length) return []
  const original = templateSlots(baseline.model), next = templateSlots(latest.model)
  const renames: TemplateRename[] = []
  for (const [path, oldName] of original) {
    const newName = next.get(path)
    if (newName && newName !== oldName && !resolveTemplate(oldName) && resolveTemplate(newName)
      && !renames.some(rename => rename.oldName === oldName && rename.newName === newName)) {
      renames.push({ oldName, newName })
    }
  }
  for (const rename of renames) renameTemplateReferences(baseline.model, rename)
  return serialize(baseline.model) === serialize(latest.model) ? renames : []
}
