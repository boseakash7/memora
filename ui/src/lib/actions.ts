// What can be done with photos and videos, in one place: the selection bar, the viewer's toolbar
// and the right-click menus all use these, so they offer the same things with the same messages.

import {
  api,
  errorMessage,
  mediaUrl,
  openWithDefaultApp,
  setWallpaper,
  showInExplorer,
  type TimelineItem,
} from './api'
import type { MenuEntry } from './contextMenu.svelte'
import { confirmDialog } from './dialogs.svelte'
import { toast } from './toast.svelte'

/** The list the items are shown in. */
export type Scope = 'all' | 'favorites' | 'album' | 'trash'

export type ActionContext = {
  /** The items to act on (at least one). */
  items: TimelineItem[]
  scope: Scope
  albumId: number | null
  /** Whether the item is a favorite, when known better than `items[0].f` (the viewer's details). */
  favorite?: boolean
  /** In the viewer: quieter (no toast for favorites), and no Open/Select/Info entries. */
  inViewer?: boolean
  /** Opens the album picker for these ids. */
  addToAlbum: (ids: number[]) => void
  /** Favorite flag changed. */
  onfavorite?: (favorite: boolean) => void
  /** The items left this list (trashed, restored, deleted, removed from the album, unfavorited). */
  onremoved?: () => void
  /** Grid only: open in the viewer, open with the info panel, select. */
  open?: () => void
  info?: () => void
  select?: () => void
}

export const countLabel = (n: number) => (n === 1 ? '1 item' : `${n.toLocaleString()} items`)

const ids = (c: ActionContext) => c.items.map((i) => i.id)

async function attempt(action: () => Promise<void>) {
  try {
    await action()
  } catch (e) {
    toast(errorMessage(e), { error: true })
  }
}

export function isFavorite(c: ActionContext): boolean {
  return c.favorite ?? c.items.every((i) => i.f)
}

// ---------- actions ----------

export const toggleFavorite = (c: ActionContext) =>
  attempt(async () => {
    const list = ids(c)
    const next = !isFavorite(c)
    c.onfavorite?.(next)
    await api.items.favorite(list, next)
    if (!next && c.scope === 'favorites') c.onremoved?.()
    if (!c.inViewer) {
      toast(next ? `Added ${countLabel(list.length)} to favorites` : `Removed ${countLabel(list.length)} from favorites`)
    }
  })

export const moveToTrash = (c: ActionContext) =>
  attempt(async () => {
    const list = ids(c)
    await api.items.trash(list)
    c.onremoved?.()
    toast(list.length === 1 ? 'Moved to trash' : `Moved ${countLabel(list.length)} to trash`, {
      action: { label: 'Undo', run: () => attempt(() => api.items.restore(list)) },
    })
  })

export const restoreFromTrash = (c: ActionContext) =>
  attempt(async () => {
    const list = ids(c)
    await api.items.restore(list)
    c.onremoved?.()
    toast(list.length === 1 ? 'Restored' : `Restored ${countLabel(list.length)}`)
  })

export const deleteForever = (c: ActionContext) =>
  attempt(async () => {
    const list = ids(c)
    const one = list.length === 1
    const ok = await confirmDialog({
      title: one ? 'Delete this item?' : `Delete ${countLabel(list.length)}?`,
      message: one
        ? 'The file is moved to the Windows Recycle Bin and removed from the library.'
        : 'The files are moved to the Windows Recycle Bin and removed from the library.',
      confirmLabel: 'Delete',
      danger: true,
    })
    if (!ok) return
    const result = await api.items.emptyTrash(list)
    if (result.deleted > 0) c.onremoved?.()
    if (result.failed.length > 0) toast(`Could not delete ${result.failed[0]}`, { error: true })
    else toast(one ? 'Moved to the Recycle Bin' : `Moved ${countLabel(result.deleted)} to the Recycle Bin`)
  })

export const removeFromAlbum = (c: ActionContext) =>
  attempt(async () => {
    const list = ids(c)
    const album = c.albumId!
    await api.albums.removeItems(album, list)
    c.onremoved?.()
    toast(`Removed ${countLabel(list.length)} from the album`, {
      action: { label: 'Undo', run: () => attempt(() => api.albums.addItems(album, list)) },
    })
  })

export const useAsCover = (c: ActionContext) =>
  attempt(async () => {
    await api.albums.update(c.albumId!, { coverMediaId: c.items[0].id })
    toast('Album cover updated')
  })

export const revealInExplorer = (c: ActionContext) =>
  attempt(async () => showInExplorer((await api.media(c.items[0].id)).path))

export const openWithDefault = (c: ActionContext) =>
  attempt(async () => openWithDefaultApp((await api.media(c.items[0].id)).path))

export const copyPaths = (c: ActionContext) =>
  attempt(async () => {
    const paths = await api.items.paths(ids(c))
    await navigator.clipboard.writeText(paths.join('\r\n'))
    toast(paths.length === 1 ? 'File path copied' : `${paths.length.toLocaleString()} file paths copied`)
  })

/** Puts the photo on the clipboard as an image (PNG, the one image type browsers can write).
 *  Uses the viewer's copy, so HEIC and RAW work and the picture is upright. */
export const copyImage = (c: ActionContext) => {
  const id = c.items[0].id
  // Started right away (inside the click), the clipboard waits for the picture.
  const png = (async () => {
    const res = await fetch(mediaUrl(id, 'display'))
    if (!res.ok) throw new Error(`The picture could not be read (${res.status})`)
    const bitmap = await createImageBitmap(await res.blob())
    const canvas = new OffscreenCanvas(bitmap.width, bitmap.height)
    canvas.getContext('2d')!.drawImage(bitmap, 0, 0)
    bitmap.close()
    return canvas.convertToBlob({ type: 'image/png' })
  })()
  return attempt(async () => {
    await navigator.clipboard.write([new ClipboardItem({ 'image/png': png })])
    toast('Image copied')
  })
}

export const makeWallpaper = (c: ActionContext) =>
  attempt(async () => {
    await setWallpaper(c.items[0].id)
    toast('Desktop background set')
  })

// ---------- menus ----------

/** Right-click menu for photos and videos: one item, a selection, or items in the Trash. */
export function itemMenu(c: ActionContext): { header?: string; entries: MenuEntry[] } {
  const n = c.items.length
  const one = n === 1
  const video = one && !!c.items[0].v
  const header = one ? undefined : `${n.toLocaleString()} items selected`

  if (c.scope === 'trash') {
    return {
      header,
      entries: [
        c.open && one ? { label: video ? 'Play' : 'Open', icon: video ? 'play' : 'openInFull', run: c.open } : 'separator',
        { label: 'Restore', icon: 'restore', run: () => restoreFromTrash(c) },
        { label: 'Delete forever', icon: 'deleteForever', danger: true, run: () => deleteForever(c) },
        'separator',
        ...(one ? [{ label: 'Show in Explorer', icon: 'folder', run: () => revealInExplorer(c) } as const] : []),
        { label: one ? 'Copy file path' : 'Copy file paths', icon: 'link', run: () => copyPaths(c) },
      ],
    }
  }

  const favorite = isFavorite(c)
  const entries: MenuEntry[] = []
  if (one && c.open) entries.push({ label: video ? 'Play' : 'Open', icon: video ? 'play' : 'openInFull', run: c.open })
  entries.push(
    {
      label: favorite ? 'Remove from favorites' : 'Add to favorites',
      icon: favorite ? 'star' : 'starOutline',
      run: () => toggleFavorite(c),
    },
    { label: 'Add to album…', icon: 'libraryAdd', run: () => c.addToAlbum(ids(c)) },
  )
  if (c.albumId !== null) {
    if (one) entries.push({ label: 'Use as album cover', icon: 'photo', run: () => useAsCover(c) })
    entries.push({ label: 'Remove from this album', icon: 'remove', run: () => removeFromAlbum(c) })
  }
  entries.push('separator')
  if (one) {
    if (!video) entries.push({ label: 'Copy image', icon: 'contentCopy', run: () => copyImage(c) })
    entries.push(
      { label: 'Copy file path', icon: 'link', run: () => copyPaths(c) },
      { label: 'Show in Explorer', icon: 'folder', run: () => revealInExplorer(c) },
      {
        label: video ? 'Open with default player' : 'Open with default app',
        icon: 'openExternal',
        run: () => openWithDefault(c),
      },
    )
    if (!video) entries.push({ label: 'Set as desktop background', icon: 'wallpaper', run: () => makeWallpaper(c) })
    entries.push('separator')
    if (c.info) entries.push({ label: 'Info', icon: 'info', shortcut: 'I', run: c.info })
    if (c.select) entries.push({ label: 'Select', icon: 'checkCircle', run: c.select })
  } else {
    entries.push({ label: 'Copy file paths', icon: 'link', run: () => copyPaths(c) })
  }
  entries.push('separator', { label: 'Move to trash', icon: 'trash', shortcut: 'Delete', run: () => moveToTrash(c) })
  return { header, entries }
}
