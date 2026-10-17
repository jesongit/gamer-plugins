import { load as parseYaml } from 'js-yaml'

// Insert a definition without rewriting the script, its comments or new syntax.
export function addScriptFunction(source, name) {
  if (!/^[a-z_\u3400-\u4dbf\u4e00-\u9fff][a-z0-9_\u3400-\u4dbf\u4e00-\u9fff]*$/.test(name)) throw new Error('函数名使用中文、小写字母、数字或下划线，不能以数字开头')
  const doc = parseYaml(source)
  if (!doc || typeof doc !== 'object' || !Array.isArray(doc.run)) throw new Error('请先编写包含 run 数组的脚本')
  if (Object.hasOwn(doc.functions || {}, name)) throw new Error('当前脚本已存在同名函数')
  const definition = `  ${name}:\n    run:\n      - log: "${name}"\n`
  if (!Object.hasOwn(doc, 'functions')) return `${source.replace(/\s*$/, '')}\nfunctions:\n${definition}`
  const header = /^functions:[ \t]*(?:#[^\n]*)?\r?$/m.exec(source)
  if (!header) throw new Error('请将 functions 写成分行缩进格式后再新增函数，原文会完整保留')
  const afterHeader = header.index + header[0].length
  const rest = source.slice(afterHeader)
  const nextField = /^\S[^\n]*:/m.exec(rest)
  const end = nextField ? afterHeader + nextField.index : source.length
  return `${source.slice(0, end).replace(/\s*$/, '')}\n${definition}${source.slice(end)}`
}

export function scriptCallSnippet(filename, functionName = '') {
  const target = functionName ? `${filename}#${functionName}` : filename
  return `  - ${JSON.stringify(target)}: {}\n`
}
