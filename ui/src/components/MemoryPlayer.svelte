<script lang="ts">
  import { onDestroy, onMount } from 'svelte'
  import { api, errorMessage, mediaUrl, type Memory } from '../lib/api'
  import { dialogs } from '../lib/dialogs.svelte'
  import Icon from '../lib/Icon.svelte'
  import { memorySubtitle } from '../lib/memories'
  import { toast } from '../lib/toast.svelte'
  import { titleBarContextMenu } from '../lib/window.svelte'
  import Viewer from './Viewer.svelte'

  /**
   * A memory as a story: one picture after another (5 s each, slowly zooming), videos muted
   * for up to 15 s. Click the right side for the next, the left for the previous; hold to pause.
   */
  let {
    id,
    onclose,
    onchanged,
  }: {
    id: number
    onclose: () => void
    /** The memory was hidden or lost photos: lists should reload. */
    onchanged?: () => void
  } = $props()

  const PHOTO_MS = 5000
  const VIDEO_MAX_MS = 15000

  let memory = $state<Memory | null>(null)
  let error = $state('')
  let index = $state(0)
  let paused = $state(false)
  let held = $state(false)
  let muted = $state(true)
  /** Time spent on the current item, ms. */
  let elapsed = $state(0)
  let video = $state<HTMLVideoElement>()
  /** The normal viewer is open on top (Open this photo). */
  let viewing = $state<number | null>(null)

  const item = $derived(memory?.items[index] ?? null)
  const stopped = $derived(paused || held || viewing !== null || !!dialogs.current)
  const length = $derived.by(() => {
    if (!item) return PHOTO_MS
    if (!item.v) return PHOTO_MS
    return Math.min(VIDEO_MAX_MS, item.d ?? VIDEO_MAX_MS)
  })

  onMount(() => {
    api.memories
      .get(id)
      .then((m) => {
        memory = m
        if (m.items.length === 0) onclose()
        api.memories.seen(id).catch(() => {})
      })
      .catch((e) => (error = errorMessage(e)))
  })

  // The clock: photos run on time; videos on their own position.
  let last = performance.now()
  let raf = requestAnimationFrame(function tick(now) {
    const dt = now - last
    last = now
    if (item && !stopped) {
      if (item.v && video && !video.paused) elapsed = video.currentTime * 1000
      else if (!item.v) elapsed += dt
      if (elapsed >= length) next()
    }
    raf = requestAnimationFrame(tick)
  })
  onDestroy(() => cancelAnimationFrame(raf))

  $effect(() => {
    if (!video) return
    if (stopped) video.pause()
    else video.play().catch(() => {})
  })

  function show(i: number) {
    if (!memory) return
    if (i >= memory.items.length) return onclose()
    index = Math.max(0, i)
    elapsed = 0
  }
  const next = () => show(index + 1)
  const prev = () => show(elapsed > 1500 || index === 0 ? index : index - 1)

  // Click: left third back, the rest forward. Holding the pointer pauses.
  let downAt = 0
  let holdTimer: ReturnType<typeof setTimeout> | undefined
  function onpointerdown(e: PointerEvent) {
    if (e.button !== 0) return
    downAt = performance.now()
    holdTimer = setTimeout(() => (held = true), 250)
  }
  function onpointerup(e: PointerEvent) {
    clearTimeout(holdTimer)
    const wasHold = held || performance.now() - downAt > 250
    held = false
    if (wasHold || e.button !== 0) return
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect()
    if (e.clientX - r.left < r.width / 3) prev()
    else next()
  }

  function onkeydown(e: KeyboardEvent) {
    if (viewing !== null || dialogs.current) return
    if (e.key === 'Escape') onclose()
    else if (e.key === ' ' || e.key === 'k') paused = !paused
    else if (e.key === 'ArrowRight') next()
    else if (e.key === 'ArrowLeft') prev()
    else if (e.key === 'm') muted = !muted
    else return
    e.preventDefault()
    e.stopPropagation()
  }

  async function removeCurrent() {
    if (!memory || !item) return
    try {
      const updated = await api.memories.remove(id, [item.id])
      memory = updated
      onchanged?.()
      toast('Removed from this memory')
      if (updated.items.length === 0) return onclose()
      show(Math.min(index, updated.items.length - 1))
    } catch (e) {
      toast(errorMessage(e), { error: true })
    }
  }

  async function hide() {
    try {
      await api.memories.setHidden(id, true)
      onchanged?.()
      onclose()
      toast('Memory hidden', {
        action: {
          label: 'Undo',
          run: async () => {
            await api.memories.setHidden(id, false)
            onchanged?.()
          },
        },
      })
    } catch (e) {
      toast(errorMessage(e), { error: true })
    }
  }

  // The next picture loads while this one shows.
  const nextItem = $derived(memory && index + 1 < memory.items.length ? memory.items[index + 1] : null)
</script>

<svelte:window onkeydowncapture={onkeydown} />

<div class="memory-player" role="dialog" aria-modal="true" aria-label={memory?.title ?? 'Memory'}>
  {#if error}
    <div class="viewer-message"><p>{error}</p></div>
  {:else if memory && item}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="memory-stage" {onpointerdown} {onpointerup} onpointerleave={() => (held = false)}>
      {#key item.id}
        {#if item.v}
          <!-- svelte-ignore a11y_media_has_caption -->
          <video
            bind:this={video}
            class="memory-media"
            src={mediaUrl(item.id, 'original')}
            poster={mediaUrl(item.id, 'thumb', item.m)}
            autoplay
            {muted}
            playsinline
            onended={next}
            onerror={() => setTimeout(next, 1500)}
          ></video>
        {:else}
          <img
            class="memory-media kenburns"
            class:reverse={index % 2 === 1}
            class:paused={stopped}
            src={mediaUrl(item.id, 'display')}
            alt=""
            draggable="false"
          />
        {/if}
      {/key}
      {#if nextItem && !nextItem.v}
        <link rel="prefetch" href={mediaUrl(nextItem.id, 'display')} />
      {/if}
    </div>

    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="memory-top" data-tauri-drag-region="deep" oncontextmenu={titleBarContextMenu}>
      <div class="memory-bars">
        {#each memory.items as it, i (it.id)}
          <span class="memory-bar">
            <span
              class="memory-bar-fill"
              style:width="{i < index ? 100 : i === index ? Math.min(100, (elapsed / length) * 100) : 0}%"
            ></span>
          </span>
        {/each}
      </div>
      <div class="memory-head">
        <button class="icon-btn" title="Close (Esc)" onclick={onclose}><Icon name="close" /></button>
        <div class="memory-titles">
          <span class="memory-title">{memory.title}</span>
          <span class="memory-sub">{memorySubtitle(memory)}</span>
        </div>
        <span class="viewer-spacer"></span>
        <button class="icon-btn" title={paused ? 'Play (Space)' : 'Pause (Space)'} onclick={() => (paused = !paused)}>
          <Icon name={paused ? 'play' : 'pause'} />
        </button>
        {#if item.v}
          <button class="icon-btn" title={muted ? 'Sound on (M)' : 'Sound off (M)'} onclick={() => (muted = !muted)}>
            <Icon name={muted ? 'volumeOff' : 'volumeUp'} />
          </button>
        {/if}
        <button class="icon-btn" title="Open this photo" onclick={() => (viewing = index)}><Icon name="openInFull" /></button>
        <button class="icon-btn" title="Remove this photo from the memory" onclick={removeCurrent}>
          <Icon name="remove" />
        </button>
        <button class="icon-btn" title="Hide this memory" onclick={hide}><Icon name="visibilityOff" /></button>
      </div>
    </div>
  {/if}
</div>

{#if memory && viewing !== null}
  <Viewer
    items={memory.items}
    index={viewing}
    onnavigate={(i) => (viewing = i)}
    onclose={() => {
      if (viewing !== null) show(viewing)
      viewing = null
    }}
  />
{/if}
