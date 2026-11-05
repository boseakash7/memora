<script lang="ts">
  import MediaPage from '../components/MediaPage.svelte'
  import { api } from '../lib/api'
  import Icon from '../lib/Icon.svelte'
  import { isBusy, library } from '../lib/library.svelte'
  import { go } from '../lib/router.svelte'

  let folderCount = $state<number | null>(null)

  $effect(() => {
    api.folders
      .list()
      .then((f) => (folderCount = f.length))
      .catch(() => {})
  })

  const busy = $derived(isBusy(library.status))
</script>

<MediaPage query={{}}>
  {#snippet empty()}
    {#if folderCount === 0}
      <div class="empty">
        <div class="empty-icon"><Icon name="photo" size={64} /></div>
        <h2>Welcome to Memora</h2>
        <p>Add the folders where you keep photos and videos. Files stay where they are; nothing is uploaded.</p>
        <button class="btn primary" onclick={() => go('settings')}>
          <Icon name="add" size={20} /> Add a folder
        </button>
      </div>
    {:else if busy}
      <div class="empty">
        <div class="empty-icon"><span class="spinner large"></span></div>
        <h2>Looking for photos and videos…</h2>
        <p>They will show up here as they are found.</p>
      </div>
    {:else}
      <div class="empty">
        <div class="empty-icon"><Icon name="photo" size={64} /></div>
        <h2>No photos or videos yet</h2>
        <p>Nothing was found in your library folders. Add another folder in Settings, or drop files into one.</p>
        <button class="btn" onclick={() => go('settings')}>Open Settings</button>
      </div>
    {/if}
  {/snippet}
</MediaPage>
