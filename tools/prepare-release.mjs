import { readFileSync, writeFileSync } from 'node:fs'
import { createHash } from 'node:crypto'
import { resolve } from 'node:path'
const [tag, output = 'dist'] = process.argv.slice(2)
const match = /^(gamer-(?:yaml|keymap|video|package-publisher|live|notify))-v(.+)$/.exec(tag || '')
if (!match) throw new Error('Expected <plugin-id>-v<manifest-version>')
const registryPath = resolve(output, 'registry.json')
const registry = JSON.parse(readFileSync(registryPath, 'utf8'))
if (registry.provenance?.sdk_snapshot_kind === 'working_tree') throw new Error('Release requires an SDK exported from committed host source')
if (!registry.plugins.some(p => p.id === match[1] && p.version === match[2])) throw new Error('Tag and manifest version differ')
const hash = bytes => createHash('sha256').update(bytes).digest('hex')
const sums = [`# plugin_commit: ${registry.provenance.plugin_commit}`, `# sdk_host_commit: ${registry.provenance.sdk_host_commit}`]
for (const p of registry.plugins) {
  if (!/^gamer-(yaml|keymap|video|package-publisher|live|notify)$/.test(p.id) || !/^[0-9A-Za-z.+-]+$/.test(p.version)) throw new Error('Invalid artifact name')
  if (p.permissions !== undefined && (!Array.isArray(p.permissions) || p.permissions.some(permission => typeof permission !== 'string'))) throw new Error('Invalid artifact permissions')
  if (p.permissions?.some(permission => permission.trim() === 'ai.connect') || ['gamer-ai', 'gamer-ocr'].includes(p.execution?.builtin_id)) throw new Error('Retired plugin functionality cannot be released')
  const name = `${p.id}-${p.version}.gplugin`
  const bytes = readFileSync(resolve(output, 'plugins', name))
  if (hash(bytes) !== p.sha256 || bytes.length !== p.size) throw new Error(`Corrupt artifact: ${name}`)
  p.download_url = `https://github.com/jesongit/gamer-plugins/releases/download/${tag}/${name}`
  sums.push(`${p.sha256}  ${name}`)
}
writeFileSync(registryPath, JSON.stringify(registry, null, 2) + '\n')
sums.push(`${hash(readFileSync(registryPath))}  registry.json`)
writeFileSync(resolve(output, 'sha256sums.txt'), sums.join('\n') + '\n')
