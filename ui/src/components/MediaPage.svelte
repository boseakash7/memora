<script lang="ts">
  import { untrack, type Snippet } from 'svelte'
  import { api, errorMessage, type Album, type TimelineItem, type TimelineQuery } from '../lib/api'
  import { confirmDialog, dialogs } from '../lib/dialogs.svelte'
  import Icon from '../lib/Icon.svelte'
  import { isBusy, library } from '../lib/library.svelte'
  import { toast } from '../lib/toast.svelte'
  import AlbumPicker from './AlbumPicker.svelte'
  import PhotoGrid from './PhotoGrid.svelte'
  import Viewer from './Viewer.svelte'

  /**
   * A grid of photos with selection, a selection toolbar and the viewer. Used by Photos,
   * Favorites, Trash, albums and search; `query` decides which items it shows.
   */
  let {
    query,
    title,
    subtitle,
    actions,
    empty,
  }: {
    query: TimelineQuery
    /** Shows a title bar above the grid. */
    title?: string
    subtitle?: string
    /** Buttons on the right of the title bar. Gets the number of items. */
    actions?: Snippet<[number]>
    /** Shown instead of the grid when there is nothing to show. */
    empty: Snippet
  } = $props()

  const scope = $derived(query.scope ?? 'all')
  const albumId = $derived(scope === 'album' ? (query.album ?? null) : null)

  let items = $state.raw<TimelineItem[] | null>(null)
  let error = $state('')
  let openId = $state<number | null>(null)
  let grid = $state<ReturnType<typeof PhotoGrid>>()
  let selected = $state.raw<ReadonlySet<number>>(new Set())
  let lastToggled: number | null = null
  let pickerIds = $state<number[] | null>(null)

  const openIndex = $derived(openId === null || !items ? -1 : items.findIndex((i) => i.id === openId))
  const selectedItems = $derived(items ? items.filter((i) => selected.has(i.id)) : [])
  const allFavorites = $derived(selectedItems.length > 0 && selectedItems.every((i) => i.f))

  // ---------- loading ----------

  let loadCount = 0

  async function load() {
    const mine = ++loadCount
    try {
      const result = await api.timeline($state.snapshot(query))
      if (mine !== loadCount) return // a newer load is on its way
      items = result
      error = ''
      // Forget selected items that are gone (trashed, removed, deleted on disk).
      if (selected.size > 0) {
        const present = new Set(result.map((i) => i.id))
        const kept = [...selected].filter((id) => present.has(id))
        if (kept.length !== selected.size) selected = new Set(kept)
      }
    } catch (e) {
      if (mine === loadCount) error = errorMessage(e)
    }
  }

  // Load on mount and whenever the query changes.
  $effect(() => {
    JSON.stringify(query)
    untrack(() => {
      selected = new Set()
      lastToggled = null
      load()
    })
  })

  // Reload when the library changes. While indexing is running, reload at most every few
  // seconds; big libraries make each reload a few MB of JSON.
  let loadedRevision = -1
  let timer: ReturnType<typeof setTimeout> | undefined
  $effect(() => {
    const s = library.status
    if (!s) return
    untrack(() => {
      if (loadedRevision === -1) {
        loadedRevision = s.revision
        return
      }
      if (s.revision === loadedRevision || timer) return
      const delay = isBusy(s) ? 3000 : 300
      timer = setTimeout(() => {
        timer = undefined
        loadedRevision = library.status?.revision ?? loadedRevision
        load()
      }, delay)
    })
  })
  $effect(() => () => clearTimeout(timer))

  // ---------- viewer ----------

  function close() {
    const index = openIndex
    openId = null
    if (index >= 0) grid?.reveal(index)
  }

  // The item went away (deleted on disk, unfavorited here...) while open.
  $effect(() => {
    if (openId !== null && items && openIndex < 0) openId = null
  })

  // ---------- selection ----------

  function toggle(index: number, range: boolean) {
    if (!items) return
    const next = new Set(selected)
    const id = items[index].id
    if (range && lastToggled !== null && lastToggled < items.length) {
      const on = !selected.has(id)
      const [a, b] = lastToggled < index ? [lastToggled, index] : [index, lastToggled]
      for (let i = a; i <= b; i++) {
        if (on) next.add(items[i].id)
        else next.delete(items[i].id)
      }
    } else if (next.has(id)) next.delete(id)
    else next.add(id)
    lastToggled = index
    selected = next
  }

  function toggleDay(start: number, end: number) {
    if (!items) return
    const ids = items.slice(start, end).map((i) => i.id)
    const next = new Set(selected)
    if (ids.every((id) => next.has(id))) ids.forEach((id) => next.delete(id))
    else ids.forEach((id) => next.add(id))
    lastToggled = start
    selected = next
  }

  function clearSelection() {
    selected = new Set()
    lastToggled = null
  }

  // ---------- actions on the selection ----------

  const count = (n: number) => (n === 1 ? '1 item' : `${n} items`)

  async function attempt(action: () => Promise<void>) {
    try {
      await action()
    } catch (e) {
      toast(errorMessage(e), { error: true })
    }
  }

  const toggleFavorite = () =>
    attempt(async () => {
      const ids = selectedItems.map((i) => i.id)
      const favorite = !allFavorites
      await api.items.favorite(ids, favorite)
      toast(favorite ? `Added ${count(ids.length)} to favorites` : `Removed ${count(ids.length)} from favorites`)
    })

  const trashSelected = () =>
    attempt(async () => {
      const ids = selectedItems.map((i) => i.id)
      if (ids.length === 0) return
      await api.items.trash(ids)
      clearSelection()
      toast(`Moved ${count(ids.length)} to trash`, {
        action: { label: 'Undo', run: () => attempt(() => api.items.restore(ids)) },
      })
    })

  const restoreSelected = () =>
    attempt(async () => {
      const ids = selectedItems.map((i) => i.id)
      await api.items.restore(ids)
      clearSelection()
      toast(`Restored ${count(ids.length)}`)
    })

  const removeFromAlbum = () =>
    attempt(async () => {
      const ids = selectedItems.map((i) => i.id)
      const album = albumId!
      await api.albums.removeItems(album, ids)
      clearSelection()
      toast(`Removed ${count(ids.length)} from the album`, {
        action: { label: 'Undo', run: () => attempt(() => api.albums.addItems(album, ids)) },
      })
    })

  const deleteForever = () =>
    attempt(async () => {
      const ids = selectedItems.map((i) => i.id)
      const ok = await confirmDialog({
        title: `Delete ${count(ids.length)}?`,
        message: 'The files are moved to the Windows Recycle Bin and removed from the library.',
        confirmLabel: 'Delete',
        danger: true,
      })
      if (!ok) return
      const result = await api.items.emptyTrash(ids)
      clearSelection()
      if (result.failed.length > 0) toast(`Could not delete ${result.failed[0]}`, { error: true })
      else toast(`Moved ${count(result.deleted)} to the Recycle Bin`)
    })

  function pickerDone(added?: Album) {
    pickerIds = null
    if (added) clearSelection()
  }

  function onkeydown(e: KeyboardEvent) {
    if (openId !== null || pickerIds || dialogs.current) return
    const target = e.target as HTMLElement | null
    if (target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.tagName === 'SELECT')) return
    if (e.key === 'Escape' && selected.size > 0) clearSelection()
    else if (e.key === 'Delete' && selected.size > 0 && scope !== 'trash') trashSelected()
    else if ((e.ctrlKey || e.metaKey) && e.key === 'a' && items && items.length > 0) selected = new Set(items.map((i) => i.id))
    else return
    e.preventDefault()
  }
</script>

<svelte:window {onkeydown} />

<div class="media-page">
  {#if title}
    <div class="page-bar">
      <div class="page-bar-title">
        <h1>{title}</h1>
        {#if subtitle}<span class="muted">{subtitle}</span>{/if}
      </div>
      <div class="page-bar-actions">{@render actions?.(items?.length ?? 0)}</div>
    </div>
  {/if}

  <div class="grid-host">
    {#if error}
      <section class="page"><p class="error">{error}</p></section>
    {:else if items === null}
      <!-- loading -->
    {:else if items.length === 0}
      <section class="page">{@render empty()}</section>
    {:else}
      <PhotoGrid
        bind:this={grid}
        {items}
        {selected}
        onopen={(i) => (openId = items![i].id)}
        ontoggle={toggle}
        ontoggleday={toggleDay}
      />
    {/if}
  </div>

  {#if selected.size > 0}
    <div class="selection-bar">
      <button class="icon-btn" title="Clear selection (Esc)" onclick={clearSelection}><Icon name="close" /></button>
      <span class="selection-count">{selected.size.toLocaleString()} selected</span>
      <span class="viewer-spacer"></span>
      {#if scope === 'trash'}
        <button class="btn" onclick={restoreSelected}><Icon name="restore" size={20} /> Restore</button>
        <button class="btn danger" onclick={deleteForever}><Icon name="deleteForever" size={20} /> Delete</button>
      {:else}
        <button class="icon-btn" title={allFavorites ? 'Remove from favorites' : 'Add to favorites'} onclick={toggleFavorite}>
          <Icon name={allFavorites ? 'star' : 'starOutline'} />
        </button>
        <button class="icon-btn" title="Add to album" onclick={() => (pickerIds = selectedItems.map((i) => i.id))}>
          <Icon name="libraryAdd" />
        </button>
        {#if albumId !== null}
          <button class="icon-btn" title="Remove from this album" onclick={removeFromAlbum}><Icon name="remove" /></button>
        {/if}
        <button class="icon-btn" title="Move to trash (Delete)" onclick={trashSelected}><Icon name="trash" /></button>
      {/if}
    </div>
  {/if}

  {#if items && openIndex >= 0}
    <Viewer
      {items}
      index={openIndex}
      {scope}
      {albumId}
      onnavigate={(i) => (openId = items![i].id)}
      onclose={close}
    />
  {/if}
  {#if pickerIds}
    <AlbumPicker ids={pickerIds} onclose={pickerDone} />
  {/if}
</div>
