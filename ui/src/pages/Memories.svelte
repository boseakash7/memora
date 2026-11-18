<script lang="ts">
  import MemoryCardTile from '../components/MemoryCardTile.svelte'
  import MemoryPlayer from '../components/MemoryPlayer.svelte'
  import { api, errorMessage, type MemoryCard } from '../lib/api'
  import { openContextMenu } from '../lib/contextMenu.svelte'
  import Icon from '../lib/Icon.svelte'
  import { KIND_SECTIONS } from '../lib/memories'
  import { toast } from '../lib/toast.svelte'

  let memories = $state<MemoryCard[] | null>(null)
  let enabled = $state(true)
  let showHidden = $state(false)
  let error = $state('')
  let playing = $state<number | null>(null)

  async function load() {
    try {
      enabled = (await api.settings.get()).memories
      memories = enabled ? await api.memories.list(showHidden) : []
      error = ''
    } catch (e) {
      error = errorMessage(e)
    }
  }

  $effect(() => {
    void showHidden
    load()
  })

  const sections = $derived(
    KIND_SECTIONS.map((s) => ({ ...s, items: (memories ?? []).filter((m) => m.kind === s.kind) })).filter(
      (s) => s.items.length > 0,
    ),
  )

  async function setHidden(m: MemoryCard, hidden: boolean) {
    try {
      await api.memories.setHidden(m.id, hidden)
      await load()
      toast(hidden ? 'Memory hidden' : 'Memory shown again', {
        action: {
          label: 'Undo',
          run: async () => {
            await api.memories.setHidden(m.id, !hidden)
            await load()
          },
        },
      })
    } catch (e) {
      toast(errorMessage(e), { error: true })
    }
  }

  function menu(m: MemoryCard, e: MouseEvent) {
    openContextMenu(e, [
      { label: 'Play', icon: 'play', run: () => (playing = m.id) },
      m.hidden
        ? { label: 'Show this memory again', icon: 'visibility', run: () => setHidden(m, false) }
        : { label: 'Hide this memory', icon: 'visibilityOff', run: () => setHidden(m, true) },
    ])
  }

  async function turnOn() {
    try {
      await api.settings.update({ memories: true })
      await api.memories.rebuild()
      await load()
    } catch (e) {
      toast(errorMessage(e), { error: true })
    }
  }
</script>

<section class="page">
  <div class="page-head-row">
    <h1>Memories</h1>
    {#if enabled}
      <label class="check-line">
        <input type="checkbox" bind:checked={showHidden} />
        Show hidden
      </label>
    {/if}
  </div>
  {#if error}<p class="error">{error}</p>{/if}

  {#if !enabled}
    <div class="empty">
      <div class="empty-icon"><Icon name="memories" size={64} /></div>
      <h2>Memories are switched off</h2>
      <p>Memora can put together stories from your library: the same days in earlier years, trips, people and the best of each year.</p>
      <button class="btn primary" onclick={turnOn}>Turn on memories</button>
    </div>
  {:else if memories && memories.length === 0}
    <div class="empty">
      <div class="empty-icon"><Icon name="memories" size={64} /></div>
      <h2>No memories yet</h2>
      <p>
        Memories appear on their own as your library grows: photos from the same days in earlier years, trips away from
        home, the people you've named, and the best of each year. Everything is worked out on this PC.
      </p>
    </div>
  {:else}
    {#each sections as s (s.kind)}
      <h2 class="memories-section">{s.title}</h2>
      <div class="memories-grid">
        {#each s.items as m (m.id)}
          <MemoryCardTile memory={m} onplay={() => (playing = m.id)} onmenu={(e) => menu(m, e)} />
        {/each}
      </div>
    {/each}
  {/if}
</section>

{#if playing !== null}
  <MemoryPlayer id={playing} onclose={() => ((playing = null), load())} onchanged={load} />
{/if}
