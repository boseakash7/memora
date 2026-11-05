<script lang="ts">
  import { api, errorMessage, mediaUrl, type Album } from '../lib/api'
  import Icon from '../lib/Icon.svelte'
  import { promptDialog } from '../lib/dialogs.svelte'
  import { toast } from '../lib/toast.svelte'

  /** Add `ids` to an existing album or a new one. */
  let { ids, onclose }: { ids: number[]; onclose: (added?: Album) => void } = $props()

  let albums = $state<Album[] | null>(null)
  let busy = $state(false)

  $effect(() => {
    api.albums
      .list()
      .then((a) => (albums = a))
      .catch((e) => {
        toast(errorMessage(e), { error: true })
        onclose()
      })
  })

  const things = $derived(ids.length === 1 ? 'item' : `${ids.length} items`)

  async function pick(album: Album) {
    if (busy) return
    busy = true
    try {
      await api.albums.addItems(album.id, ids)
      toast(`Added ${things === 'item' ? '1 item' : things} to “${album.name}”`)
      onclose(album)
    } catch (e) {
      toast(errorMessage(e), { error: true })
      busy = false
    }
  }

  async function create() {
    const name = await promptDialog({ title: 'New album', label: 'Album name', confirmLabel: 'Create' })
    if (!name) return
    busy = true
    try {
      const album = await api.albums.create(name, ids)
      toast(`Created “${album.name}” with ${album.count} ${album.count === 1 ? 'item' : 'items'}`)
      onclose(album)
    } catch (e) {
      toast(errorMessage(e), { error: true })
      busy = false
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault()
      e.stopPropagation()
      onclose()
    }
  }
</script>

<svelte:window onkeydowncapture={onkeydown} />

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="modal-backdrop" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div class="modal picker" role="dialog" aria-modal="true" aria-label="Add to album">
    <h2>Add to album</h2>
    <button class="picker-row" onclick={create} disabled={busy}>
      <span class="picker-thumb new"><Icon name="add" size={28} /></span>
      <span class="picker-name">New album</span>
    </button>
    <div class="picker-list">
      {#if albums === null}
        <div class="pad"><span class="spinner"></span></div>
      {:else}
        {#each albums as album (album.id)}
          <button class="picker-row" onclick={() => pick(album)} disabled={busy}>
            <span class="picker-thumb">
              {#if album.coverId !== null}
                <img src={mediaUrl(album.coverId, 'thumb', album.coverM ?? undefined)} alt="" draggable="false" />
              {:else}
                <Icon name="albums" size={24} />
              {/if}
            </span>
            <span class="picker-name">{album.name}</span>
            <span class="muted">{album.count}</span>
          </button>
        {/each}
      {/if}
    </div>
    <div class="modal-actions">
      <button class="btn" onclick={() => onclose()}>Cancel</button>
    </div>
  </div>
</div>
