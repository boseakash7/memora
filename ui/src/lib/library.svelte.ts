import { listen } from '@tauri-apps/api/event'
import { api, type LibraryStatus } from './api'

/** Live indexing status, pushed by the app as `index-progress` events. */
export const library = $state<{ status: LibraryStatus | null }>({ status: null })

export function isBusy(s: LibraryStatus | null): boolean {
  return !!s && (s.scanning || s.pending > 0)
}

let started = false

export function watchLibrary() {
  if (started) return
  started = true
  listen<LibraryStatus>('index-progress', (e) => {
    library.status = e.payload
  })
  api.library
    .status()
    .then((s) => (library.status ??= s))
    .catch(() => {})
}
