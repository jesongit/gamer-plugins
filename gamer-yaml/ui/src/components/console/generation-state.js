export function readAuthoringLocation(packageId) {
  try {
    const value = JSON.parse(globalThis.localStorage?.getItem(`gamer-yaml.authoring.${packageId}`) || '{}')
    return value && typeof value === 'object' && !Array.isArray(value) ? value : {}
  } catch { return {} }
}
export function rememberAuthoringLocation(packageId, update) {
  if (!packageId) return
  try {
    const key = `gamer-yaml.authoring.${packageId}`
    const value = JSON.stringify({ ...readAuthoringLocation(packageId), ...update })
    if (globalThis.localStorage?.getItem(key) !== value) globalThis.localStorage?.setItem(key, value)
  } catch { /* Browser preferences must not prevent reading saved scripts. */ }
}

export function sourceDiff(before = '', after = '') {
  const left = before.split('\n'), right = after.split('\n')
  let start = 0; while (start < left.length && start < right.length && left[start] === right[start]) start++
  let endLeft = left.length, endRight = right.length
  while (endLeft > start && endRight > start && left[endLeft - 1] === right[endRight - 1]) { endLeft--; endRight-- }
  if (start === left.length && start === right.length) return '没有源码变化'
  return [`@@ 第 ${start + 1} 行 @@`, ...left.slice(start, endLeft).map(line => `- ${line}`), ...right.slice(start, endRight).map(line => `+ ${line}`)].join('\n')
}
