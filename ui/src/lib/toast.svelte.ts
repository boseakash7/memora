/** Short messages at the bottom of the window, optionally with one action such as Undo. */

export type Toast = {
  id: number
  message: string
  error: boolean
  action?: { label: string; run: () => void | Promise<void> }
}

export const toasts = $state<{ list: Toast[] }>({ list: [] })

let nextId = 1

export function toast(message: string, options: { action?: Toast['action']; error?: boolean } = {}) {
  const t: Toast = { id: nextId++, message, error: options.error ?? false, action: options.action }
  toasts.list = [...toasts.list.slice(-2), t]
  setTimeout(() => dismiss(t.id), options.action || options.error ? 8000 : 4000)
}

export function dismiss(id: number) {
  toasts.list = toasts.list.filter((t) => t.id !== id)
}
