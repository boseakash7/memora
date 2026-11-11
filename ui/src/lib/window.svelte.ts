// The app window: its state for the custom title bar, and the window buttons' actions.
// In the dev browser (no Tauri) there is no window to control and the buttons are hidden.

import { invoke, isTauri } from '@tauri-apps/api/core'
import { getCurrentWindow } from '@tauri-apps/api/window'

export const inTauri = isTauri()

export const appWindow = $state({ maximized: false, focused: true })

if (inTauri) {
  const w = getCurrentWindow()
  const refresh = () =>
    w
      .isMaximized()
      .then((m) => (appWindow.maximized = m))
      .catch(() => {})
  refresh()
  w.onResized(refresh)
  w.isFocused()
    .then((f) => (appWindow.focused = f))
    .catch(() => {})
  w.onFocusChanged(({ payload }) => (appWindow.focused = payload))
  document.documentElement.classList.add('custom-titlebar')
}

export function minimizeWindow() {
  getCurrentWindow().minimize()
}

export function toggleMaximizeWindow() {
  getCurrentWindow().toggleMaximize()
}

export function closeWindow() {
  getCurrentWindow().close()
}

/** Opens the Windows window menu (Restore, Move, Size, …) at a point in the window, in CSS pixels. */
export function showSystemMenu(x: number, y: number) {
  if (inTauri) invoke('show_system_menu', { x, y }).catch(() => {})
}

const CONTROLS = 'a, button, input, select, textarea, label, [role="button"], [tabindex]:not([tabindex="-1"])'

/**
 * `oncontextmenu` for a title bar area: a right-click on an empty part opens the window menu,
 * like on a native title bar. Right-clicks on its buttons and fields are left alone.
 */
export function titleBarContextMenu(e: MouseEvent) {
  if (!inTauri) return
  const target = e.target as Element | null
  const bar = e.currentTarget as Element
  const control = target?.closest(CONTROLS)
  if (control && bar.contains(control)) return
  e.preventDefault()
  showSystemMenu(e.clientX, e.clientY)
}
