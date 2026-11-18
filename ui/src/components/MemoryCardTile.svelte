<script lang="ts">
  import { mediaUrl, type MemoryCard } from '../lib/api'
  import Icon from '../lib/Icon.svelte'
  import { memorySubtitle } from '../lib/memories'

  let {
    memory,
    size = 'normal',
    onplay,
    onmenu,
  }: {
    memory: MemoryCard
    /** `small` in the Photos carousel. */
    size?: 'normal' | 'small'
    onplay: () => void
    onmenu?: (e: MouseEvent) => void
  } = $props()
</script>

<button
  class="memory-card"
  class:small={size === 'small'}
  class:seen={memory.seen}
  class:hidden-memory={memory.hidden}
  onclick={onplay}
  oncontextmenu={onmenu}
  title={`${memory.title} · ${memorySubtitle(memory)}`}
>
  {#if memory.coverId !== null}
    <img src={mediaUrl(memory.coverId, 'thumb', memory.coverM ?? undefined)} alt="" draggable="false" decoding="async" />
  {/if}
  <span class="memory-card-text">
    <span class="memory-card-title">{memory.title}</span>
    <span class="memory-card-sub">{memorySubtitle(memory)}</span>
  </span>
  {#if memory.hidden}<span class="memory-card-flag"><Icon name="visibilityOff" size={16} /> Hidden</span>{/if}
</button>
