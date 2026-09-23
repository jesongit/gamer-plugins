import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { resolve } from 'node:path'
const root = fileURLToPath(new URL('../', import.meta.url))
const requested = process.argv.slice(2)
const ids = requested.length ? requested : ['gamer-yaml', 'gamer-keymap', 'gamer-video']
for (const id of ids) {
  if (!['gamer-yaml', 'gamer-keymap', 'gamer-video'].includes(id)) throw new Error('Unknown plugin')
  for (const args of [['install', '--frozen-lockfile'], ['build']]) {
    const result = spawnSync('pnpm', args, { cwd: resolve(root, id, 'ui'), stdio: 'inherit', shell: process.platform === 'win32' })
    if (result.status !== 0) process.exit(result.status || 1)
  }
}
