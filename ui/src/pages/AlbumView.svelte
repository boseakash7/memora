<script lang="ts">
  import { untrack } from 'svelte'
  import MediaPage from '../components/MediaPage.svelte'
  import { api, errorMessage, type Album } from '../lib/api'
  import { confirmDialog, promptDialog } from '../lib/dialogs.svelte'
  import Icon from '../lib/Icon.svelte'
  import { library } from '../lib/library.svelte'
  import { go } from '../lib/router.svelte'
  import { toast } from '../lib/toast.svelte'

  let { id }: { id: number } = $props()

  let album = $state<Album | null>(null)

  async function load() {
    try {
      album = await api.albums.get(id)
    } catch {
      go('albums') // deleted, or never existed
    }
  }

  $effect(() => {
    id
    untrack(load)
  })

  let seen: number | null = null
  $effect(() => {
    const revision = library.status?.revision ?? null
    untrack(() => {
      if (seen !== null && revision !== seen) load()
      seen = revision
    })
  })

  async function rename() {
    if (!album) return
    const name = await promptDialog({
      title: 'Rename album',
      label: 'Album name',
      value: album.name,
      confirmLabel: 'Save',
    })
    if (!name || name === album.name) return
    try {
      album = await api.albums.update(id, { name })
    } catch (e) {
      toast(errorMessage(e), { error: true })
    }
  }

  async function remove() {
    if (!album) return
    const ok = await confirmDialog({
      title: `Delete “${album.name}”?`,
      message: 'Only the album is deleted. Its photos and videos stay in your library.',
      confirmLabel: 'Delete album',
      danger: true,
    })
    if (!ok) return
    try {
      await api.albums.remove(id)
      go('albums')
    } catch (e) {
      toast(errorMessage(e), { error: true })
    }
  }
</script>

{#key id}
  <MediaPage
    query={{ scope: 'album', album: id }}
    title={album?.name ?? 'Album'}
    subtitle={album ? `${album.count.toLocaleString()} ${album.count === 1 ? 'item' : 'items'}` : ''}
  >
    {#snippet actions()}
      <button class="btn" onclick={rename}><Icon name="edit" size={20} /> Rename</button>
      <button class="btn" onclick={remove}><Icon name="trash" size={20} /> Delete album</button>
    {/snippet}
    {#snippet empty()}
      <div class="empty">
        <div class="empty-icon"><Icon name="albums" size={56} /></div>
        <h2>This album is empty</h2>
        <p>In Photos, select some photos and choose “Add to album”.</p>
        <button class="btn" onclick={() => go('photos')}>Go to Photos</button>
      </div>
    {/snippet}
  </MediaPage>
{/key}
