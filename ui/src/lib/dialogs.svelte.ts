/** Modal questions: `confirmDialog` for yes/no, `promptDialog` for a line of text. Rendered by Dialogs.svelte. */

export type DialogState =
  | {
      kind: 'confirm'
      title: string
      message: string
      confirmLabel: string
      danger: boolean
      resolve: (ok: boolean) => void
    }
  | {
      kind: 'prompt'
      title: string
      label: string
      value: string
      confirmLabel: string
      resolve: (value: string | null) => void
    }

export const dialogs = $state<{ current: DialogState | null }>({ current: null })

export function confirmDialog(opts: {
  title: string
  message: string
  confirmLabel: string
  danger?: boolean
}): Promise<boolean> {
  return new Promise((resolve) => {
    dialogs.current = { kind: 'confirm', danger: false, ...opts, resolve }
  })
}

/** Resolves to the entered text, or null if cancelled. */
export function promptDialog(opts: {
  title: string
  label: string
  value?: string
  confirmLabel: string
}): Promise<string | null> {
  return new Promise((resolve) => {
    dialogs.current = { kind: 'prompt', value: '', ...opts, resolve }
  })
}
