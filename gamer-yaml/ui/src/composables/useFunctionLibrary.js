import { reactive } from 'vue'
/** Library listing only. Source parsing and serialization belong to the host. */
export function useFunctionLibrary({ api }) {
  let serial = 0
  return reactive({ list: [], async refresh(packageId) {
    const request = ++serial
    const list = packageId ? await api.listFunctions(packageId) : []
    if (request === serial) this.list = list || []
    return list
  } })
}
