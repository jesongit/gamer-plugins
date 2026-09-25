import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { createHash } from 'node:crypto'
import { spawnSync } from 'node:child_process'
const script = fileURLToPath(new URL('./prepare-release.mjs', import.meta.url))
function fixture() {
  const root = mkdtempSync(resolve(tmpdir(), 'gamer-plugin-release-'))
  mkdirSync(resolve(root, 'plugins'))
  const bytes = Buffer.from('verified fixture archive')
  writeFileSync(resolve(root, 'plugins/gamer-yaml-1.2.3.gplugin'), bytes)
  writeFileSync(resolve(root, 'registry.json'), JSON.stringify({ provenance: { plugin_commit: 'a'.repeat(40), sdk_host_commit: 'b'.repeat(40) }, plugins: [{ id: 'gamer-yaml', version: '1.2.3', size: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') }] }))
  return root
}
test('release tag must match a packaged manifest', () => {
  const root = fixture()
  assert.notEqual(spawnSync(process.execPath, [script, 'gamer-yaml-v1.2.4', root]).status, 0)
  assert.equal(spawnSync(process.execPath, [script, 'gamer-yaml-v1.2.3', root]).status, 0)
  const registry = JSON.parse(readFileSync(resolve(root, 'registry.json')))
  assert.equal(registry.plugins[0].download_url, 'https://github.com/jesongit/gamer-plugins/releases/download/gamer-yaml-v1.2.3/gamer-yaml-1.2.3.gplugin')
  const digest = createHash('sha256').update(readFileSync(resolve(root, 'registry.json'))).digest('hex')
  assert.ok(readFileSync(resolve(root, 'sha256sums.txt'), 'utf8').includes(`${digest}  registry.json`))
})
test('changed artifact is rejected before publishing the catalog', () => {
  const root = fixture()
  const before = readFileSync(resolve(root, 'registry.json'))
  writeFileSync(resolve(root, 'plugins/gamer-yaml-1.2.3.gplugin'), 'tampered')
  assert.notEqual(spawnSync(process.execPath, [script, 'gamer-yaml-v1.2.3', root]).status, 0)
  assert.deepEqual(readFileSync(resolve(root, 'registry.json')), before)
})


test('publisher tag accepts its packaged builtin catalog', () => {
  const root = fixture()
  const registry = JSON.parse(readFileSync(resolve(root, 'registry.json')))
  registry.plugins[0].id = 'gamer-package-publisher'
  writeFileSync(resolve(root, 'plugins/gamer-package-publisher-1.2.3.gplugin'), readFileSync(resolve(root, 'plugins/gamer-yaml-1.2.3.gplugin')))
  writeFileSync(resolve(root, 'registry.json'), JSON.stringify(registry))
  assert.equal(spawnSync(process.execPath, [script, 'gamer-package-publisher-v1.2.3', root]).status, 0)
})
