// The app's right-click menu (one at a time). Rendered by ContextMenu.svelte in App.svelte.

import type { IconName } from './icons'

export type MenuEntry =
  | {
      label: string
      icon?: IconName
      /** Keyboard shortcut shown on the right, e.g. "Delete". */
      shortcut?: string
      danger?: boolean
      disabled?: boolean
      run: () => void
    }
  | 'separator'

export type OpenMenu = {
  x: number
  y: number
  /** Shown above the entries, e.g. "3 items selected". */
  header?: string
  entries: MenuEntry[]
  /** Opened from the keyboard (Shift+F10, Menu key): the first entry gets focus. */
  keyboard: boolean
}

export const contextMenu = $state<{ current: OpenMenu | null }>({ current: null })

/**
 * Opens a menu for a `contextmenu` event (mouse, Shift+F10 or the Menu key). Keyboard-opened
 * menus appear at the element that has focus.
 */
export function openContextMenu(e: MouseEvent, entries: MenuEntry[], header?: string) {
  e.preventDefault()
  e.stopPropagation()
  const el = e.currentTarget instanceof Element ? e.currentTarget : (e.target as Element)
  const r = el.getBoundingClientRect()
  const inside = e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom
  // Keyboard-opened: no pointer position inside the element.
  const keyboard = !inside || (e.clientX === 0 && e.clientY === 0)
  const x = keyboard ? r.left + Math.min(24, r.width / 2) : e.clientX
  const y = keyboard ? r.top + Math.min(24, r.height / 2) : e.clientY
  contextMenu.current = { x, y, header, entries: tidy(entries), keyboard }
}

export function closeContextMenu() {
  contextMenu.current = null
}

/** No separators at the ends or next to each other. */
function tidy(entries: MenuEntry[]): MenuEntry[] {
  const out: MenuEntry[] = []
  for (const e of entries) {
    if (e === 'separator' && (out.length === 0 || out[out.length - 1] === 'separator')) continue
    out.push(e)
  }
  if (out[out.length - 1] === 'separator') out.pop()
  return out
}
