<script lang="ts">
  import { slidePhoto } from '../lib/motion.svelte'
  import { vaultUrl, type TimelineItem, type VaultItem } from '../lib/api'
  import { dateTimeLabel, formatBytes, formatDuration, formatOffset } from '../lib/dates'
  import { dialogs } from '../lib/dialogs.svelte'
  import Icon from '../lib/Icon.svelte'
  import { appWindow, setFullscreen, titleBarContextMenu } from '../lib/window.svelte'
  import VideoPlayer from './VideoPlayer.svelte'

  /**
   * Viewer for the locked folder. Pictures come decrypted from the PC and are never cached.
   * No file actions (Show in Explorer, open with another app): the files are encrypted.
   */
  let {
    items,
    index,
    onnavigate,
    onclose,
    onfavorite,
    onmoveout,
    ondelete,
  }: {
    items: VaultItem[]
    index: number
    onnavigate: (index: number) => void
    onclose: () => void
    onfavorite: (item: VaultItem) => void
    onmoveout: (item: VaultItem) => void
    ondelete: (item: VaultItem) => void
  } = $props()

  const item = $derived(items[index])
  /** What the video player needs to know (the id is only used for its own state). */
  const asTimeline = $derived<TimelineItem>({
    id: index + 1,
    w: item.width ?? 0,
    h: item.height ?? 0,
    t: item.takenAt,
    v: item.kind === 'video',
    d: item.durationMs ?? undefined,
    m: 0,
  })

  let showInfo = $state(false)
  let loaded = $state(false)
  let idle = $state(false)
  let stageW = $state(0)
  let stageH = $state(0)

  $effect(() => {
    void item.id
    loaded = false
    idle = false
  })

  const box = $derived.by(() => {
    if (!item.width || !item.height || !stageW || !stageH) return { w: stageW, h: stageH }
    const s = Math.min(stageW / item.width, stageH / item.height)
    return { w: Math.round(item.width * s), h: Math.round(item.height * s) }
  })
  const when = $derived(item.takenAt ? dateTimeLabel(item.takenAt) : null)
  const folder = $derived(item.originalPath.replace(/[\\/][^\\/]*$/, ''))

  $effect(() => () => {
    if (appWindow.fullscreen) setFullscreen(false)
  })

  /** Which way the last Next / Previous went, for the slide. */
  let dir = $state(1)

  /** The first moment after opening, while the photo zooms in (see motion.css). */
  let opening = $state(true)
  $effect(() => {
    const t = setTimeout(() => (opening = false), 500)
    return () => clearTimeout(t)
  })

  function go(delta: number) {
    const next = index + delta
    dir = Math.sign(delta) || 1
    if (next >= 0 && next < items.length) onnavigate(next)
  }

  function onkeydown(e: KeyboardEvent) {
    if (dialogs.current) return
    if (e.key === 'ArrowLeft') go(-1)
    else if (e.key === 'ArrowRight') go(1)
    else if (e.key === 'i') showInfo = !showInfo
    else if (e.key === 'Escape') {
      if (appWindow.fullscreen) setFullscreen(false)
      else onclose()
    } else return
    e.preventDefault()
  }
</script>

<svelte:window {onkeydown} />

<div class="viewer" class:opening class:idle={idle && item.kind === 'video'} role="dialog" aria-modal="true" aria-label="Locked folder viewer">
  <div class="viewer-main">
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="viewer-bar" data-tauri-drag-region="deep" oncontextmenu={titleBarContextMenu}>
      <button class="icon-btn" title="Back (Esc)" onclick={onclose}><Icon name="arrowBack" /></button>
      <span class="vault-chip"><Icon name="lock" size={16} /> Locked folder</span>
      <span class="viewer-spacer"></span>
      <button
        class="icon-btn"
        title={item.favorite ? 'Remove from favorites' : 'Add to favorites'}
        onclick={() => onfavorite(item)}
      >
        <Icon name={item.favorite ? 'star' : 'starOutline'} />
      </button>
      <button class="icon-btn" title="Move out of the locked folder…" onclick={() => onmoveout(item)}>
        <Icon name="moveOut" />
      </button>
      <button class="icon-btn" title="Delete forever" onclick={() => ondelete(item)}><Icon name="deleteForever" /></button>
      <button class="icon-btn" class:active={showInfo} title="Info (I)" onclick={() => (showInfo = !showInfo)}>
        <Icon name="info" />
      </button>
    </div>

    <div class="viewer-stage" bind:clientWidth={stageW} bind:clientHeight={stageH}>
      {#key item.id}
        <div class="viewer-slide" in:slidePhoto={{ dir }} out:slidePhoto={{ dir, out: true }}>
          {#if item.kind === 'video'}
            <VideoPlayer
              item={asTimeline}
              {box}
              keys={!dialogs.current}
              bind:idle
              source={{ original: vaultUrl(item.id, 'original'), poster: vaultUrl(item.id, 'thumb') }}
            />
          {:else}
            <div class="viewer-photo" style:width="{box.w}px" style:height="{box.h}px">
              <img class="viewer-thumb" src={vaultUrl(item.id, 'thumb')} alt="" draggable="false" />
              <img
                class="viewer-full"
                class:loaded
                src={vaultUrl(item.id, 'display')}
                alt={item.name}
                draggable="false"
                onload={() => (loaded = true)}
              />
            </div>
          {/if}
        </div>
      {/key}
      {#if index > 0}
        <button class="viewer-nav prev" title="Previous (←)" onclick={() => go(-1)}><Icon name="chevronLeft" size={32} /></button>
      {/if}
      {#if index < items.length - 1}
        <button class="viewer-nav next" title="Next (→)" onclick={() => go(1)}><Icon name="chevronRight" size={32} /></button>
      {/if}
    </div>
  </div>

  {#if showInfo}
    <aside class="info-panel">
      <div class="info-head">
        <button class="icon-btn" title="Close" onclick={() => (showInfo = false)}><Icon name="close" /></button>
        <h2>Info</h2>
      </div>
      <h3 class="info-title">Details</h3>
      {#if when}
        <div class="info-row">
          <Icon name="calendar" />
          <div>
            <div>{when.date}</div>
            <div class="muted">{when.time}{item.tzOffset !== null ? ` ${formatOffset(item.tzOffset)}` : ''}</div>
          </div>
        </div>
      {/if}
      <div class="info-row">
        <Icon name={item.kind === 'video' ? 'play' : 'photo'} />
        <div>
          <div class="info-name">{item.name}</div>
          <div class="muted">
            {#if item.width && item.height}{item.width} × {item.height} ·{/if}
            {formatBytes(item.size)}{item.durationMs ? ` · ${formatDuration(item.durationMs)}` : ''}
          </div>
        </div>
      </div>
      {#if item.camera}
        <div class="info-row">
          <Icon name="camera" />
          <div>{item.camera}</div>
        </div>
      {/if}
      {#if item.lat !== null && item.lon !== null}
        <div class="info-row">
          <Icon name="explore" />
          <div>{item.lat.toFixed(5)}, {item.lon.toFixed(5)}</div>
        </div>
      {/if}
      <div class="info-row">
        <Icon name="lock" />
        <div>
          <div>Encrypted in the locked folder</div>
          <div class="muted">Was in {folder}</div>
          <div class="muted">Moved in {new Date(item.addedAt).toLocaleDateString()}</div>
        </div>
      </div>
    </aside>
  {/if}
</div>
