<script lang="ts">
  import type { MemoryCard } from '../lib/api'
  import Icon from '../lib/Icon.svelte'
  import MemoryCardTile from './MemoryCardTile.svelte'

  /** The row of memories above the Photos timeline. */
  let { memories, onplay }: { memories: MemoryCard[]; onplay: (id: number) => void } = $props()

  let row = $state<HTMLDivElement>()
  let atStart = $state(true)
  let atEnd = $state(false)

  function update() {
    if (!row) return
    atStart = row.scrollLeft < 4
    atEnd = row.scrollLeft + row.clientWidth >= row.scrollWidth - 4
  }

  $effect(() => {
    void memories.length
    queueMicrotask(update)
  })

  function scroll(dir: number) {
    row?.scrollBy({ left: dir * row.clientWidth * 0.8, behavior: 'smooth' })
  }
</script>

<div class="memories-carousel">
  <div class="memories-head">
    <h2>Memories</h2>
    <a class="memories-all" href="#/memories">See all</a>
  </div>
  <div class="memories-row-wrap">
    <div class="memories-row" bind:this={row} onscroll={update}>
      {#each memories as m (m.id)}
        <MemoryCardTile memory={m} size="small" onplay={() => onplay(m.id)} />
      {/each}
    </div>
    {#if !atStart}
      <button class="memories-arrow left" title="Back" onclick={() => scroll(-1)}><Icon name="chevronLeft" /></button>
    {/if}
    {#if !atEnd}
      <button class="memories-arrow right" title="More" onclick={() => scroll(1)}><Icon name="chevronRight" /></button>
    {/if}
  </div>
</div>
