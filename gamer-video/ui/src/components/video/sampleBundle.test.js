import { describe, expect, it, beforeAll } from 'vitest'
import { webcrypto } from 'node:crypto'
import { zipSync, strToU8 } from 'fflate'
import { canonicalSampleJson, inspectSampleArchive, inspectZipDirectory } from './sampleBundle'
beforeAll(() => { Object.defineProperty(globalThis, 'crypto', { value: webcrypto, configurable: true }) })
const hash = async b => [...new Uint8Array(await crypto.subtle.digest('SHA-256', b))].map(n => n.toString(16).padStart(2, '0')).join('')
async function fixture() {
  const bytes = new Uint8Array([1, 2, 3])
  const manifest = { schema_version: 1, id: 'sample-1', content_sha256: '', frames: [{ path: 'frames/a.png' }], files: [{ path: 'frames/a.png', size: bytes.length, sha256: await hash(bytes) }] }
  manifest.content_sha256 = await hash(strToU8(canonicalSampleJson(manifest)))
  return zipSync({ 'manifest.json': strToU8(JSON.stringify(manifest)), 'frames/a.png': bytes })
}
describe('portable sample import', () => {
  it('checks fingerprint and files before exposing a bundle', async () => {
    const bundle = await inspectSampleArchive(await fixture())
    expect(bundle.manifest.id).toBe('sample-1')
    expect(bundle.files).toEqual([{ path: 'frames/a.png', base64: 'AQID' }])
  })
  it('rejects traversal before decompression', () => {
    expect(() => inspectZipDirectory(zipSync({ '../outside': new Uint8Array([1]) }))).toThrow()
  })
  it('rejects malicious expanded size budgets before allocation', async () => {
    const bytes = await fixture(), view = new DataView(bytes.buffer)
    for (let i = 0; i < bytes.length - 46; i++) {
      if (view.getUint32(i, true) === 0x02014b50) { view.setUint32(i + 24, 0xffffffff, true); break }
    }
    expect(() => inspectZipDirectory(bytes)).toThrow()
  })
  it('rejects mismatched per-disk count before fflate can read extra entries', () => {
    const bytes = zipSync({ 'manifest.json': strToU8('{}'), 'unvalidated.bin': new Uint8Array([1, 2, 3]) })
    new DataView(bytes.buffer).setUint16(bytes.length - 22 + 10, 1, true)
    expect(() => inspectZipDirectory(bytes)).toThrow('计数')
  })
  it('rejects real deflate output larger than its forged advertised size', async () => {
    const body = new Uint8Array(1024 * 1024)
    const manifest = { schema_version: 1, id: 'sample-1', content_sha256: '', frames: [{ path: 'frames/a.png' }], files: [{ path: 'frames/a.png', size: 1, sha256: await hash(new Uint8Array([0])) }] }
    manifest.content_sha256 = await hash(strToU8(canonicalSampleJson(manifest)))
    const bytes = zipSync({ 'manifest.json': strToU8(JSON.stringify(manifest)), 'frames/a.png': body })
    const view = new DataView(bytes.buffer)
    for (let i = 0; i < bytes.length - 46; i++) {
      if (view.getUint32(i, true) === 0x02014b50 && view.getUint32(i + 24, true) === body.length) view.setUint32(i + 24, 1, true)
      if (view.getUint32(i, true) === 0x04034b50 && view.getUint32(i + 22, true) === body.length) view.setUint32(i + 22, 1, true)
    }
    await expect(inspectSampleArchive(bytes)).rejects.toThrow('实际解压大小')
  })
  it('rejects oversized files before reading them into browser memory', async () => {
    let read = false
    await expect(inspectSampleArchive({ size: 129 * 1024 * 1024, arrayBuffer() { read = true; return new ArrayBuffer(0) } })).rejects.toThrow('128 MiB')
    expect(read).toBe(false)
  })
  it('sorts nested keys for the cross-language content fingerprint', () => {
    expect(canonicalSampleJson({ z: 1, a: [{ y: 2, b: 3 }] })).toBe('{"a":[{"b":3,"y":2}],"z":1}')
  })
})
