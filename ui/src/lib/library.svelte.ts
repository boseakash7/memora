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

/** What indexing is doing, with a count that goes down as it works. Indexing takes the
 * whole library through one step at a time, so the overall `pending` count would stand
 * still until the last step. */
export function indexingStep(s: LibraryStatus): string {
  if (s.toRead > 0) return `Reading files… ${s.toRead.toLocaleString()} left`
  if (s.toThumbnail > 0) return `Making thumbnails… ${s.toThumbnail.toLocaleString()} left`
  if (s.toHash > 0) return `Finishing up… ${s.toHash.toLocaleString()} left`
  return `Indexing… ${s.pending.toLocaleString()} left`
}
