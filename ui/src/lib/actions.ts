// What can be done with photos and videos, in one place: the selection bar, the viewer's toolbar
// and the right-click menus all use these, so they offer the same things with the same messages.

import {
  api,
  ApiError,
  clearWebCache,
  errorMessage,
  frameUrl,
  mediaUrl,
  openWithDefaultApp,
  setWallpaper,
  showInExplorer,
  type TimelineItem,
} from './api'
import type { MenuEntry } from './contextMenu.svelte'
import { confirmDialog, promptDialog } from './dialogs.svelte'
import { go } from './router.svelte'
import { toast } from './toast.svelte'

/** The list the items are shown in. */
export type Scope = 'all' | 'favorites' | 'album' | 'archive' | 'trash'

export type ActionContext = {
  /** The items to act on (at least one). */
  items: TimelineItem[]
  scope: Scope
  albumId: number | null
  /** Whether the item is a favorite, when known better than `items[0].f` (the viewer's details). */
  favorite?: boolean
  /** Whether the items are archived, when known better than the scope (the viewer's details). */
  archived?: boolean
  /** In the viewer: quieter (no toast for favorites), and no Open/Select/Info entries. */
  inViewer?: boolean
  /** Opens the album picker for these ids. */
  addToAlbum: (ids: number[]) => void
  /** Favorite flag changed. */
  onfavorite?: (favorite: boolean) => void
  /** The items left this list (trashed, restored, deleted, (un)archived, removed from the album, unfavorited). */
  onremoved?: () => void
  /** Open the photo editor (the viewer and the grid offer it for one photo). */
  edit?: () => void
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

export function isArchived(c: ActionContext): boolean {
  return c.archived ?? c.scope === 'archive'
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

/** Archive the items, or bring them back when they are archived. */
export const toggleArchive = (c: ActionContext) =>
  attempt(async () => {
    const list = ids(c)
    const one = list.length === 1
    if (isArchived(c)) {
      await api.items.unarchive(list)
      c.onremoved?.()
      toast(one ? 'Removed from the archive' : `Removed ${countLabel(list.length)} from the archive`, {
        action: { label: 'Undo', run: () => attempt(() => api.items.archive(list)) },
      })
    } else {
      await api.items.archive(list)
      c.onremoved?.()
      toast(one ? 'Archived' : `Archived ${countLabel(list.length)}`, {
        action: { label: 'Undo', run: () => attempt(() => api.items.unarchive(list)) },
      })
    }
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

/** Puts the video frame at `t` seconds on the clipboard, full size (the PC decodes it). */
export const copyVideoFrame = (id: number, t: number) => {
  // Started right away (inside the click), the clipboard waits for the picture.
  const png = fetch(frameUrl(id, t)).then((res) => {
    if (!res.ok) throw new Error(`The frame could not be read (${res.status})`)
    return res.blob()
  })
  return attempt(async () => {
    await navigator.clipboard.write([new ClipboardItem({ 'image/png': png })])
    toast('Frame copied')
  })
}

export const makeWallpaper = (c: ActionContext) =>
  attempt(async () => {
    await setWallpaper(c.items[0].id)
    toast('Desktop background set')
  })

/**
 * Ask for the locked folder's password until it opens. Returns false if cancelled, or if
 * there is no locked folder yet (then the Locked folder page is opened to set one up).
 */
export async function ensureVaultUnlocked(): Promise<boolean> {
  const state = await api.vault.state()
  if (!state.exists) {
    const ok = await confirmDialog({
      title: 'Set up the locked folder',
      message: 'Items in the locked folder are encrypted and open only in Memora with your password. Set it up first?',
      confirmLabel: 'Set up',
    })
    if (ok) go('vault')
    return false
  }
  if (state.unlocked) return true
  let error: string | undefined
  for (;;) {
    const password = await promptDialog({
      title: 'Unlock the locked folder',
      label: 'Password',
      password: true,
      error,
      confirmLabel: 'Unlock',
    })
    if (password === null) return false
    try {
      await api.vault.unlock(password)
      return true
    } catch (e) {
      if (e instanceof ApiError && e.status === 403) error = 'Wrong password. Try again.'
      else throw e
    }
  }
}

/** Encrypt the items into the locked folder; the originals are deleted. */
export const moveToVault = (c: ActionContext) =>
  attempt(async () => {
    const list = ids(c)
    const one = list.length === 1
    if (!(await ensureVaultUnlocked())) return
    const ok = await confirmDialog({
      title: one ? 'Move to the locked folder?' : `Move ${countLabel(list.length)} to the locked folder?`,
      message:
        'They are encrypted and open only in Memora with your password. They leave the library, Memories and ' +
        'TV sharing, and the original files are deleted from their folder for good (not to the Recycle Bin). ' +
        'To get them back, use Move out on the Locked folder page.',
      confirmLabel: 'Move',
    })
    if (!ok) return
    const hint = toast(one ? 'Moving to the locked folder…' : `Moving ${countLabel(list.length)} to the locked folder…`, {
      persist: true,
    })
    const result = await api.vault.moveIn(list).finally(hint.dismiss)
    // Pictures shown before the move may be in the webview's cache; clear it.
    if (result.moved > 0) {
      c.onremoved?.()
      await clearWebCache()
    }
    if (result.failed.length > 0) {
      toast(`${result.moved ? `Moved ${countLabel(result.moved)}. ` : ''}Not moved: ${result.failed[0]}`, { error: true })
    } else {
      toast(one ? 'Moved to the locked folder' : `Moved ${countLabel(result.moved)} to the locked folder`, {
        action: { label: 'Open', run: () => go('vault') },
      })
    }
  })

// ---------- menus ----------

export function archiveEntry(c: ActionContext): MenuEntry {
  return isArchived(c)
    ? { label: 'Unarchive', icon: 'unarchive', shortcut: 'Shift+A', run: () => toggleArchive(c) }
    : { label: 'Archive', icon: 'archive', shortcut: 'Shift+A', run: () => toggleArchive(c) }
}

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
  if (one && !video && c.edit) entries.push({ label: 'Edit', icon: 'tune', run: c.edit })
  entries.push(
    {
      label: favorite ? 'Remove from favorites' : 'Add to favorites',
      icon: favorite ? 'star' : 'starOutline',
      run: () => toggleFavorite(c),
    },
    { label: 'Add to album…', icon: 'libraryAdd', run: () => c.addToAlbum(ids(c)) },
    archiveEntry(c),
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
  entries.push(
    'separator',
    { label: 'Move to locked folder', icon: 'lock', run: () => moveToVault(c) },
    { label: 'Move to trash', icon: 'trash', shortcut: 'Delete', run: () => moveToTrash(c) },
  )
  return { header, entries }
}
