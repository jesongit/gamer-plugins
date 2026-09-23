import { readFileSync } from 'node:fs'
import { createHash } from 'node:crypto'
import { resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const lock = JSON.parse(readFileSync(resolve(root, 'sdk/lock.json'), 'utf8'))
if (!/^[a-f0-9]{40}$/.test(lock.commit) || lock.schema_version !== 1) throw new Error('Invalid SDK lock')
for (const file of lock.files) {
  if (!/^[\w./-]+$/.test(file.path) || file.path.split('/').includes('..')) throw new Error('Invalid SDK path')
  const bytes = readFileSync(resolve(root, 'sdk', file.path))
  if (createHash('sha256').update(bytes).digest('hex') !== file.sha256) throw new Error(`SDK snapshot changed: ${file.path}`)
}
console.log(`SDK verified: ${lock.commit} (${lock.files.length} files)`)
