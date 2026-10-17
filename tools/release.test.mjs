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

test('live plugin tag accepts its packaged builtin catalog', () => {
  const root = fixture()
  const registry = JSON.parse(readFileSync(resolve(root, 'registry.json')))
  registry.plugins[0].id = 'gamer-live'
  writeFileSync(resolve(root, 'plugins/gamer-live-1.2.3.gplugin'), readFileSync(resolve(root, 'plugins/gamer-yaml-1.2.3.gplugin')))
  writeFileSync(resolve(root, 'registry.json'), JSON.stringify(registry))
  assert.equal(spawnSync(process.execPath, [script, 'gamer-live-v1.2.3', root]).status, 0)
})

test('notify release accepts the complete catalog and emits immutable notification URLs', () => {
  const root = fixture()
  const registry = JSON.parse(readFileSync(resolve(root, 'registry.json')))
  const yaml = registry.plugins[0]
  registry.plugins.push({ ...yaml, id: 'gamer-notify', version: '0.1.0' })
  writeFileSync(resolve(root, 'plugins/gamer-notify-0.1.0.gplugin'), readFileSync(resolve(root, 'plugins/gamer-yaml-1.2.3.gplugin')))
  writeFileSync(resolve(root, 'registry.json'), JSON.stringify(registry))
  assert.equal(spawnSync(process.execPath, [script, 'gamer-notify-v0.1.0', root]).status, 0)
  const released = JSON.parse(readFileSync(resolve(root, 'registry.json')))
  assert.equal(released.plugins[1].download_url, 'https://github.com/jesongit/gamer-plugins/releases/download/gamer-notify-v0.1.0/gamer-notify-0.1.0.gplugin')
  assert.ok(readFileSync(resolve(root, 'sha256sums.txt'), 'utf8').includes(`${yaml.sha256}  gamer-notify-0.1.0.gplugin`))
})

test('OCR release validates all seven current official artifacts and emits immutable URLs and checksums', () => {
  const root = fixture()
  const registry = JSON.parse(readFileSync(resolve(root, 'registry.json')))
  const yaml = registry.plugins[0]
  const bytes = readFileSync(resolve(root, 'plugins/gamer-yaml-1.2.3.gplugin'))
  for (const id of ['gamer-keymap', 'gamer-video', 'gamer-package-publisher', 'gamer-live', 'gamer-notify', 'gamer-ocr']) {
    registry.plugins.push({ ...yaml, id, version: '0.1.0' })
    writeFileSync(resolve(root, `plugins/${id}-0.1.0.gplugin`), bytes)
  }
  writeFileSync(resolve(root, 'registry.json'), JSON.stringify(registry))
  const before = readFileSync(resolve(root, 'registry.json'))
  assert.notEqual(spawnSync(process.execPath, [script, 'gamer-ocr-v0.1.1', root]).status, 0)
  assert.deepEqual(readFileSync(resolve(root, 'registry.json')), before)
  assert.equal(spawnSync(process.execPath, [script, 'gamer-ocr-v0.1.0', root]).status, 0)
  const released = JSON.parse(readFileSync(resolve(root, 'registry.json')))
  const sums = readFileSync(resolve(root, 'sha256sums.txt'), 'utf8')
  assert.equal(released.plugins.length, 7)
  for (const plugin of released.plugins) {
    const name = `${plugin.id}-${plugin.version}.gplugin`
    assert.equal(plugin.download_url, `https://github.com/jesongit/gamer-plugins/releases/download/gamer-ocr-v0.1.0/${name}`)
    assert.ok(sums.includes(`${plugin.sha256}  ${name}`))
    assert.deepEqual(readFileSync(resolve(root, 'plugins', name)), bytes)
  }
  const digest = createHash('sha256').update(readFileSync(resolve(root, 'registry.json'))).digest('hex')
  assert.ok(sums.includes(`${digest}  registry.json`))
})

test('release rejects retired AI and unknown artifacts before modifying the catalog', () => {
  const root = fixture()
  const registry = JSON.parse(readFileSync(resolve(root, 'registry.json')))
  const yaml = registry.plugins[0]
  const bytes = readFileSync(resolve(root, 'plugins/gamer-yaml-1.2.3.gplugin'))
  for (const id of ['gamer-ai', 'gamer-unknown']) {
    registry.plugins.push({ ...yaml, id, version: '0.1.0' })
    writeFileSync(resolve(root, `plugins/${id}-0.1.0.gplugin`), bytes)
  }
  writeFileSync(resolve(root, 'registry.json'), JSON.stringify(registry))
  const before = readFileSync(resolve(root, 'registry.json'))
  assert.notEqual(spawnSync(process.execPath, [script, 'gamer-yaml-v1.2.3', root]).status, 0)
  assert.deepEqual(readFileSync(resolve(root, 'registry.json')), before)
})

test('retired AI release tags are rejected before catalog changes', () => {
  const root = fixture()
  const before = readFileSync(resolve(root, 'registry.json'))
  assert.notEqual(spawnSync(process.execPath, [script, 'gamer-ai-v0.1.0', root]).status, 0)
  assert.deepEqual(readFileSync(resolve(root, 'registry.json')), before)
})

test('valid official IDs cannot release retired permissions or the removed builtin', () => {
  for (const retiredMetadata of [{ permissions: [' ai.connect '] }, { execution: { kind: 'builtin', builtin_id: 'gamer-ai' } }, { permissions: 'ai.connect' }]) {
    const root = fixture()
    const registry = JSON.parse(readFileSync(resolve(root, 'registry.json')))
    Object.assign(registry.plugins[0], retiredMetadata)
    writeFileSync(resolve(root, 'registry.json'), JSON.stringify(registry))
    const before = readFileSync(resolve(root, 'registry.json'))
    assert.notEqual(spawnSync(process.execPath, [script, 'gamer-yaml-v1.2.3', root]).status, 0)
    assert.deepEqual(readFileSync(resolve(root, 'registry.json')), before)
  }
})

test('uncommitted SDK snapshots cannot be relabeled as release artifacts', () => {
  const root = fixture()
  const registry = JSON.parse(readFileSync(resolve(root, 'registry.json')))
  registry.provenance.sdk_snapshot_kind = 'working_tree'
  writeFileSync(resolve(root, 'registry.json'), JSON.stringify(registry))
  const before = readFileSync(resolve(root, 'registry.json'))
  assert.notEqual(spawnSync(process.execPath, [script, 'gamer-yaml-v1.2.3', root]).status, 0)
  assert.deepEqual(readFileSync(resolve(root, 'registry.json')), before)
})
