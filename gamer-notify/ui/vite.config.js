import { fileURLToPath } from 'node:url'
import { pluginUiConfig } from '../../sdk/ui/module-build.mjs'
export default await pluginUiConfig(import.meta.url, { hostSourceRoot: fileURLToPath(new URL('../../../web/src/', import.meta.url)) })
