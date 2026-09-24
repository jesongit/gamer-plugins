/** Conservative reference typing, mirrored by host/reference_types.rs. */
import type { Cell, FunctionLibraryModel, ParamDecl, Program, Step } from './model'
import { isRefCell, childStepLists } from './model'
import { diag, type Diagnostic } from './diagnostics'
import { normalizeParamType } from './schema'
import type { ValidationContext } from './validation'

type Env = Map<string, string>
const matches = new Set(['find', 'find_any', 'wait_find', 'tap_template'])
const booleans = new Set(['wait_disappear', 'eq', 'ne', 'gt', 'ge', 'lt', 'le'])
const strings = new Set(['string', 'template', 'key'])

function initial(params: ParamDecl[], vars: Record<string, unknown>): Env {
  const env = new Map(params.map(p => [p.name, normalizeParamType(p.type)])) as Env
  for (const [name, value] of Object.entries(vars)) {
    env.set(name, value === null ? 'any' : Array.isArray(value) ? 'list'
      : typeof value === 'number' ? Number.isInteger(value) ? 'integer' : 'number' : typeof value)
  }
  return env
}

function refType(path: string, env: Env): string {
  const [head, ...fields] = path.split('.')
  let type = env.get(head) || 'any'
  const fieldsOfMatch: Record<string, string> = { center: 'point', template: 'template', score: 'number',
    index: 'integer', x: 'integer', y: 'integer', width: 'integer', height: 'integer', region: 'object' }
  for (const field of fields) {
    type = type === 'match' ? fieldsOfMatch[field] || 'any'
      : type === 'point' && ['x', 'y'].includes(field) ? 'number' : 'any'
  }
  return type
}

function accepts(actual: string, expected: string, tap: boolean): boolean {
  if (actual === 'any' || expected === 'any' || actual === expected) return true
  if (actual === 'match') return expected === 'object' || (tap && expected === 'point')
  if (strings.has(actual) && (strings.has(expected) || expected === 'duration')) return true
  if (actual === 'duration' && (strings.has(expected) || ['number', 'integer'].includes(expected))) return true
  if (['integer', 'number'].includes(actual) && ['number', 'integer', 'duration'].includes(expected)) return true
  return (['object', 'list'].includes(actual) && expected === 'point')
    || (actual === 'point' && ['object', 'list'].includes(expected))
}

function check(cell: Cell, expected: string, tap: boolean, path: string, field: string, env: Env, out: Diagnostic[]): void {
  if (!isRefCell(cell)) return
  const actual = refType(cell.ref, env)
  expected = normalizeParamType(expected)
  if (accepts(actual, expected, tap)) return
  const hint = actual === 'match' && expected === 'template'
    ? `；模板名称请用 $${cell.ref}.template，点击该结果请使用 tap` : ''
  out.push(diag('yaml.args.ref_type', path, field,
    `引用 $${cell.ref} 的类型为 ${actual === 'match' ? '匹配结果' : actual}，此处需要 ${expected}${hint}`))
}

function forget(steps: Step[], env: Env): void {
  for (const step of steps) {
    if (step.kind === 'call' && step.as) env.set(step.as, 'any')
    if (step.kind === 'match_templates') {
      for (const c of step.cases) {
        const previous = c.as ? env.get(c.as) : undefined
        forget(c.body, env)
        if (c.as) { if (previous === undefined) env.delete(c.as); else env.set(c.as, previous) }
      }
      forget(step.else, env)
    } else for (const child of childStepLists(step)) forget(child.list, env)
  }
}

function walk(steps: Step[], base: string, env: Env, ctx: ValidationContext, out: Diagnostic[]): void {
  steps.forEach((step, index) => {
    const path = `${base}[${index}]`
    if (step.kind === 'call') {
      const params = ctx.resolveParams?.(step.fn) ?? []
      params.forEach((param, i) => {
        const cell = step.args.kind === 'map' ? step.args.entries[param.name]
          : step.args.kind === 'value' && i === 0 ? step.args.cell : null
        if (!cell) return
        check(cell, param.type, step.fn === 'tap' && param.name === 'position', path, param.name, env, out)
        if (param.items?.type && !isRefCell(cell) && Array.isArray(cell.lit)) {
          cell.lit.forEach((item, index) => {
            if (typeof item === 'string' && item.startsWith('$') && !item.startsWith('$$')) {
              check({ ref: item.slice(1) }, param.items!.type, false, path, `${param.name}[${index}]`, env, out)
            }
          })
        }
      })
      if (step.as) env.set(step.as, matches.has(step.fn) ? 'match' : booleans.has(step.fn) ? 'boolean' : 'any')
    } else if (step.kind === 'match_templates') {
      check(step.threshold, 'number', false, path, 'threshold', env, out)
      step.cases.forEach((c, index) => {
        check(c.template, 'template', false, path, `cases[${index}].template`, env, out)
        const local = new Map(env)
        if (c.as) local.set(c.as, 'match')
        walk(c.body, `${path}.cases[${index}].do`, local, ctx, out)
      })
      walk(step.else, `${path}.else`, new Map(env), ctx, out)
      forget([step], env)
    } else if (step.kind === 'repeat') {
      check(step.times, 'integer', false, path, 'repeat', env, out)
      forget(step.body, env)
      walk(step.body, `${path}.do`, new Map(env), ctx, out)
    } else if (step.kind === 'if') {
      walk(step.then, `${path}.then`, new Map(env), ctx, out)
      walk(step.else, `${path}.else`, new Map(env), ctx, out)
      forget([step], env)
    }
  })
}

export function scriptReferenceTypes(model: Program, ctx: ValidationContext): Diagnostic[] {
  const out: Diagnostic[] = []
  walk(model.run, 'run', initial(model.params, model.vars), ctx, out)
  return out
}

export function functionReferenceTypes(model: FunctionLibraryModel, ctx: ValidationContext): Diagnostic[] {
  const out: Diagnostic[] = []
  const localCtx = { ...ctx, resolveParams: (name: string) => model.functions.find(f => f.name === name)?.params ?? ctx.resolveParams?.(name) ?? null }
  for (const fn of model.functions) walk(fn.run, `functions.${fn.name}.run`, initial(fn.params, fn.vars), localCtx, out)
  return out
}
