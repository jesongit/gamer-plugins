// A click can be retargeted to the backdrop when a drag starts in its dialog.
// Only dismiss after a primary press and release on the backdrop itself.
const gestures = new WeakMap()
const moved = (start, event) => (event.clientX - start.x) ** 2 + (event.clientY - start.y) ** 2 > 25

export const vBackdropDismiss = {
  mounted(element, binding) {
    const state = { dismiss: binding.value, start: null, click: false }
    const reset = () => { state.start = null; state.click = false }
    const down = event => {
      reset()
      if (event.target === element && event.button === 0 && event.isPrimary !== false) {
        state.start = { id: event.pointerId, x: event.clientX, y: event.clientY, dragged: false }
      }
    }
    const move = event => {
      if (state.start?.id === event.pointerId && moved(state.start, event)) state.start.dragged = true
    }
    const up = event => {
      const start = state.start
      state.start = null
      state.click = !!start && start.id === event.pointerId && event.target === element
        && event.button === 0 && !start.dragged && !moved(start, event)
    }
    const click = event => {
      const allowed = state.click
      reset()
      if (allowed && event.target === element && event.button === 0) state.dismiss?.(event)
    }
    const listeners = [['pointerdown', down, true], ['pointermove', move, true], ['pointerup', up, true], ['pointercancel', reset, true], ['click', click, false]]
    for (const [name, handler, capture] of listeners) element.addEventListener(name, handler, capture)
    const view = element.ownerDocument.defaultView
    view?.addEventListener('blur', reset)
    state.cleanup = () => {
      for (const [name, handler, capture] of listeners) element.removeEventListener(name, handler, capture)
      view?.removeEventListener('blur', reset)
    }
    gestures.set(element, state)
  },
  updated(element, binding) {
    const state = gestures.get(element)
    if (state) state.dismiss = binding.value
  },
  beforeUnmount(element) {
    gestures.get(element)?.cleanup()
    gestures.delete(element)
  },
}
