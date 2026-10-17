import { pluginMessageChannel } from '../../../../../../web/src/workspace/plugin-messages'

// Publishing changes formal resources without going through the template editor.
// Every consumer shares this package-scoped signal; it carries no source content.
export const yamlResourcesChanged = pluginMessageChannel('gamer-yaml:resources-changed')
export function notifyYamlResourcesChanged(packageId, scriptId = '') {
  yamlResourcesChanged.packageId = packageId
  yamlResourcesChanged.scriptId = scriptId
  yamlResourcesChanged.seq++
  const packageChanged = pluginMessageChannel('core:package-resources-changed')
  packageChanged.packageId = packageId
  packageChanged.seq++
}
