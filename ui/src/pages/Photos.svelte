<script lang="ts">
  import MediaPage from '../components/MediaPage.svelte'
  import MemoriesCarousel from '../components/MemoriesCarousel.svelte'
  import MemoryPlayer from '../components/MemoryPlayer.svelte'
  import { api, type MemoryCard } from '../lib/api'
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

  /** Up to 12 memories for the row above the timeline: on this day first, then unseen ones. */
  let memories = $state<MemoryCard[]>([])
  let playing = $state<number | null>(null)
  const MEMORIES_HEIGHT = 268

  function loadMemories() {
    api.memories
      .list()
      .then((m) => (memories = m.slice(0, 12)))
      .catch(() => {})
  }
  loadMemories()
</script>

{#snippet carousel()}
  <MemoriesCarousel {memories} onplay={(id) => (playing = id)} />
{/snippet}

{#if playing !== null}
  <MemoryPlayer id={playing} onclose={() => ((playing = null), loadMemories())} onchanged={loadMemories} />
{/if}

<MediaPage query={{}} top={memories.length > 0 ? carousel : undefined} topHeight={MEMORIES_HEIGHT}>
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
