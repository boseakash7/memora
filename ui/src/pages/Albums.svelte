<script lang="ts">
  import { untrack } from 'svelte'
  import { api, errorMessage, mediaUrl, type Album } from '../lib/api'
  import { promptDialog } from '../lib/dialogs.svelte'
  import Icon from '../lib/Icon.svelte'
  import { library } from '../lib/library.svelte'
  import { goAlbum } from '../lib/router.svelte'
  import { toast } from '../lib/toast.svelte'

  let albums = $state<Album[] | null>(null)
  let error = $state('')

  async function load() {
    try {
      albums = await api.albums.list()
      error = ''
    } catch (e) {
      error = errorMessage(e)
    }
  }

  $effect(() => {
    load()
  })

  // Counts and covers change with the library (trash, new photos, album edits).
  let seen: number | null = null
  $effect(() => {
    const revision = library.status?.revision ?? null
    untrack(() => {
      if (seen !== null && revision !== seen) load()
      seen = revision
    })
  })

  async function create() {
    const name = await promptDialog({ title: 'New album', label: 'Album name', confirmLabel: 'Create' })
    if (!name) return
    try {
      const album = await api.albums.create(name)
      goAlbum(album.id)
    } catch (e) {
      toast(errorMessage(e), { error: true })
    }
  }
</script>

<section class="page">
  <div class="page-head">
    <h1>Albums</h1>
    <button class="btn" onclick={create}><Icon name="add" size={20} /> New album</button>
  </div>

  {#if error}
    <p class="error">{error}</p>
  {:else if albums === null}
    <!-- loading -->
  {:else if albums.length === 0}
    <div class="empty">
      <div class="empty-icon"><Icon name="albums" size={56} /></div>
      <h2>No albums yet</h2>
      <p>Group photos and videos into albums. The files stay where they are.</p>
      <button class="btn primary" onclick={create}><Icon name="add" size={20} /> New album</button>
    </div>
  {:else}
    <div class="album-grid">
      {#each albums as album (album.id)}
        <a class="album-card" href={`#/album/${album.id}`}>
          <span class="album-cover">
            {#if album.coverId !== null}
              <img
                src={mediaUrl(album.coverId, 'thumb', album.coverM ?? undefined)}
                alt=""
                draggable="false"
                loading="lazy"
              />
            {:else}
              <Icon name="albums" size={48} />
            {/if}
          </span>
          <span class="album-name">{album.name}</span>
          <span class="muted">{album.count.toLocaleString()} {album.count === 1 ? 'item' : 'items'}</span>
        </a>
      {/each}
    </div>
  {/if}
</section>
