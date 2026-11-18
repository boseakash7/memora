<script lang="ts">
  import * as act from '../lib/actions'
  import {
    api,
    countryName,
    errorMessage,
    faceUrl,
    mediaUrl,
    type AlbumRef,
    type Face,
    type Media,
    type MediaInfo,
    type TimelineItem,
  } from '../lib/api'
  import { codecLabel, dateTimeLabel, formatBitrate, formatBytes, formatDuration, formatOffset } from '../lib/dates'
  import { openContextMenu } from '../lib/contextMenu.svelte'
  import { dialogs } from '../lib/dialogs.svelte'
  import Icon from '../lib/Icon.svelte'
  import { goAlbum, goPerson } from '../lib/router.svelte'
  import { toast } from '../lib/toast.svelte'
  import { appWindow, setFullscreen, titleBarContextMenu } from '../lib/window.svelte'
  import AlbumPicker from './AlbumPicker.svelte'
  import VideoPlayer from './VideoPlayer.svelte'

  let {
    items,
    index,
    scope = 'all',
    albumId = null,
    info: openWithInfo = false,
    onnavigate,
    onclose,
  }: {
    items: TimelineItem[]
    index: number
    /** The list being viewed: decides which actions are offered. */
    scope?: act.Scope
    albumId?: number | null
    /** Start with the info panel open. */
    info?: boolean
    onnavigate: (index: number) => void
    onclose: () => void
  } = $props()

  const item = $derived(items[index])

  let details = $state<Media | null>(null)
  let info = $state<MediaInfo | null>(null)
  // svelte-ignore state_referenced_locally
  let showInfo = $state(openWithInfo)
  let loaded = $state(false)
  /** A video is playing and the mouse is still: the bar and controls fade out. */
  let idle = $state(false)
  let albums = $state<AlbumRef[]>([])
  /** Faces of people (not hidden) in this item. */
  let faces = $state<Face[]>([])
  let picking = $state(false)

  let stageW = $state(0)
  let stageH = $state(0)
  let scale = $state(1)
  let tx = $state(0)
  let ty = $state(0)
  let drag: { x: number; y: number } | null = null

  // Reset per item, and load its details for the info panel and the file actions.
  $effect(() => {
    const id = item.id
    scale = 1
    tx = ty = 0
    loaded = false
    idle = false
    details = null
    albums = []
    faces = []
    loadFaces(id)
    api
      .media(id)
      .then((m) => {
        if (item.id === id) details = m
      })
      .catch(() => {})
    loadAlbums(id)
  })

  // Exposure, codec and place come from reading the file, so only fetch them while the panel is open.
  $effect(() => {
    if (!showInfo) return
    const id = item.id
    info = null
    api
      .mediaInfo(id)
      .then((i) => {
        if (item.id === id) info = i
      })
      .catch(() => {})
  })

  function loadFaces(id: number) {
    api.faces
      .of(id)
      .then((f) => {
        if (item.id === id) faces = f.filter((x) => x.person && !x.person.hidden)
      })
      .catch(() => {})
  }

  /** "Not this person": the face leaves the person and stays out of automatic grouping. */
  async function notThisPerson(face: Face) {
    const before = face.person
    if (!before) return
    const id = item.id
    try {
      await api.faces.setPerson(face.id, null)
      loadFaces(id)
      toast(`Removed from ${before.name ?? 'this person'}`, {
        action: {
          label: 'Undo',
          run: async () => {
            try {
              await api.faces.setPerson(face.id, before.id)
              loadFaces(id)
            } catch (e) {
              toast(errorMessage(e), { error: true })
            }
          },
        },
      })
    } catch (e) {
      toast(errorMessage(e), { error: true })
    }
  }

  function loadAlbums(id: number) {
    api.items
      .albums(id)
      .then((a) => {
        if (item.id === id) albums = a
      })
      .catch(() => {})
  }

  // Warm the cache for the neighbours so arrow keys feel instant.
  $effect(() => {
    for (const n of [index + 1, index - 1]) {
      const it = items[n]
      if (it && !it.v) new Image().src = mediaUrl(it.id, 'display')
    }
  })

  /**
   * On-screen size of the photo or video: fit to the stage, keeping aspect ratio.
   * `w`/`h` are already as displayed (rotation applied), which is how the webview draws
   * rotated phone videos too.
   */
  const box = $derived.by(() => {
    if (!item.w || !item.h || !stageW || !stageH) return { w: stageW, h: stageH }
    const s = Math.min(stageW / item.w, stageH / item.h)
    return { w: Math.round(item.w * s), h: Math.round(item.h * s) }
  })

  // Full screen belongs to the viewer: leave it when the viewer closes.
  $effect(() => () => {
    if (appWindow.fullscreen) setFullscreen(false)
  })

  const hasPrev = $derived(index > 0)
  const hasNext = $derived(index < items.length - 1)

  function go(delta: number) {
    const n = index + delta
    if (n >= 0 && n < items.length) onnavigate(n)
  }

  /** After removing the current item from the list: show the next one, or close. */
  function advance() {
    if (index < items.length - 1) onnavigate(index + 1)
    else if (index > 0) onnavigate(index - 1)
    else onclose()
  }

  /** Actions on the item shown; the same ones as in the grid's menu and selection bar. */
  function context(): act.ActionContext {
    return {
      items: [item],
      scope,
      albumId,
      favorite,
      archived,
      inViewer: true,
      addToAlbum: () => (picking = true),
      onfavorite: (f) => {
        if (details) details.favorite = f
      },
      onremoved: advance,
      info: () => (showInfo = !showInfo),
    }
  }
  const run = (action: (c: act.ActionContext) => void) => () => action(context())

  function onmenu(e: MouseEvent) {
    const { entries } = act.itemMenu(context())
    openContextMenu(e, entries)
  }

  function onkeydown(e: KeyboardEvent) {
    if (picking || dialogs.current) return
    if (e.key === 'ArrowLeft') go(-1)
    else if (e.key === 'ArrowRight') go(1)
    else if (e.key === 'i') showInfo = !showInfo
    else if (e.key === 'Delete' && scope !== 'trash') act.moveToTrash(context())
    else if (e.key === 'A' && e.shiftKey && !e.ctrlKey && scope !== 'trash') act.toggleArchive(context())
    else if (e.key === 'Escape') {
      if (appWindow.fullscreen) setFullscreen(false)
      else if (scale > 1) resetZoom()
      else onclose()
    } else return
    e.preventDefault()
  }

  function resetZoom() {
    scale = 1
    tx = ty = 0
  }

  /** Zoom around point (cx, cy), given relative to the stage centre. */
  function zoomTo(next: number, cx: number, cy: number) {
    next = Math.min(8, Math.max(1, next))
    if (next === 1) return resetZoom()
    tx = cx - ((cx - tx) * next) / scale
    ty = cy - ((cy - ty) * next) / scale
    scale = next
  }

  function centre(e: MouseEvent) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect()
    return [e.clientX - r.left - r.width / 2, e.clientY - r.top - r.height / 2] as const
  }

  function onwheel(e: WheelEvent) {
    if (item.v) return
    e.preventDefault()
    const [cx, cy] = centre(e)
    zoomTo(scale * Math.exp(-e.deltaY * 0.0015), cx, cy)
  }

  function ondblclick(e: MouseEvent) {
    if (item.v) return
    const [cx, cy] = centre(e)
    if (scale > 1) resetZoom()
    else zoomTo(2.5, cx, cy)
  }

  function onpointerdown(e: PointerEvent) {
    if (scale <= 1 || e.button !== 0) return
    drag = { x: e.clientX, y: e.clientY }
    ;(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId)
  }

  function onpointermove(e: PointerEvent) {
    if (!drag) return
    tx += e.clientX - drag.x
    ty += e.clientY - drag.y
    drag = { x: e.clientX, y: e.clientY }
  }

  const favorite = $derived(details ? details.favorite : (item.f ?? false))
  const archived = $derived(details ? details.archived : scope === 'archive')
  const when = $derived(details ? dateTimeLabel(details.takenAt) : null)
  const camera = $derived(
    details ? [details.cameraMake, details.cameraModel].filter(Boolean).join(' ').trim() || null : null,
  )
  const megapixels = $derived(
    details?.width && details.height ? ((details.width * details.height) / 1e6).toFixed(1) : null,
  )
  const fileType = $derived(details?.name.includes('.') ? details.name.split('.').pop()!.toUpperCase() : null)
  const place = $derived(
    info?.place ? [info.place.city, info.place.admin1, countryName(info.place.country)].filter(Boolean).join(', ') : null,
  )
  const exposure = $derived(
    info
      ? [
          info.fNumber && `f/${info.fNumber}`,
          info.exposure && `${info.exposure} s`,
          info.focalLength && `${Math.round(info.focalLength)} mm`,
          info.iso && `ISO ${info.iso}`,
        ]
          .filter(Boolean)
          .join(' · ')
      : '',
  )
  const videoLine = $derived(
    info
      ? [
          info.videoCodec && codecLabel(info.videoCodec),
          info.fps && `${info.fps} fps`,
          info.bitrate && formatBitrate(info.bitrate),
        ]
          .filter(Boolean)
          .join(' · ')
      : '',
  )
</script>

<svelte:window {onkeydown} />

<div class="viewer" class:idle={idle && item.v} role="dialog" aria-modal="true" aria-label="Photo viewer">
  <div class="viewer-main">
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="viewer-bar" data-tauri-drag-region="deep" oncontextmenu={titleBarContextMenu}>
      <button class="icon-btn" title="Back (Esc)" onclick={onclose}><Icon name="arrowBack" /></button>
      <span class="viewer-spacer"></span>
      {#if scope === 'trash'}
        <button class="btn viewer-btn" onclick={run(act.restoreFromTrash)}><Icon name="restore" size={20} /> Restore</button>
        <button class="btn viewer-btn" onclick={run(act.deleteForever)}>
          <Icon name="deleteForever" size={20} /> Delete
        </button>
      {:else}
        <button
          class="icon-btn"
          title={favorite ? 'Remove from favorites' : 'Add to favorites'}
          onclick={run(act.toggleFavorite)}
        >
          <Icon name={favorite ? 'star' : 'starOutline'} />
        </button>
        <button class="icon-btn" title="Add to album" onclick={() => (picking = true)}>
          <Icon name="libraryAdd" />
        </button>
        {#if albumId !== null}
          <button class="icon-btn" title="Use as album cover" onclick={run(act.useAsCover)}><Icon name="photo" /></button>
        {/if}
        <button
          class="icon-btn"
          title={archived ? 'Unarchive (Shift+A)' : 'Archive (Shift+A)'}
          onclick={run(act.toggleArchive)}
        >
          <Icon name={archived ? 'unarchive' : 'archive'} />
        </button>
        <button class="icon-btn" title="Move to trash (Delete)" onclick={run(act.moveToTrash)}><Icon name="trash" /></button>
      {/if}
      <button class="icon-btn" title="Show in Explorer" onclick={run(act.revealInExplorer)}>
        <Icon name="folder" />
      </button>
      <button class="icon-btn" title="Open with default app" onclick={run(act.openWithDefault)}>
        <Icon name="openExternal" />
      </button>
      <button class="icon-btn" class:active={showInfo} title="Info (I)" onclick={() => (showInfo = !showInfo)}>
        <Icon name="info" />
      </button>
    </div>

    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="viewer-stage"
      class:zoomed={scale > 1}
      bind:clientWidth={stageW}
      bind:clientHeight={stageH}
      {onwheel}
      {ondblclick}
      {onpointerdown}
      oncontextmenu={onmenu}
      {onpointermove}
      onpointerup={() => (drag = null)}
    >
      {#key item.id}
        {#if item.v}
          <VideoPlayer
            {item}
            {box}
            keys={!picking && !dialogs.current}
            bind:idle
            onopenexternal={run(act.openWithDefault)}
          />
        {:else}
          <div
            class="viewer-photo"
            style:width="{box.w}px"
            style:height="{box.h}px"
            style:transform="translate({tx}px, {ty}px) scale({scale})"
          >
            <img class="viewer-thumb" src={mediaUrl(item.id, 'thumb', item.m)} alt="" draggable="false" />
            <img
              class="viewer-full"
              class:loaded
              src={mediaUrl(item.id, 'display')}
              alt={details?.name ?? ''}
              draggable="false"
              onload={() => (loaded = true)}
            />
          </div>
        {/if}
      {/key}

      {#if hasPrev}
        <button class="viewer-nav prev" title="Previous (←)" onclick={() => go(-1)}><Icon name="chevronLeft" size={32} /></button>
      {/if}
      {#if hasNext}
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
      {#if faces.length > 0}
        <h3 class="info-title">People</h3>
        <div class="face-chips">
          {#each faces as f (f.id)}
            <span class="face-chip">
              <button class="face-chip-main" onclick={() => goPerson(f.person!.id)} title="Show all photos of this person">
                <img src={faceUrl(f.id)} alt="" draggable="false" />
                <span class:muted={!f.person!.name}>{f.person!.name ?? 'Unnamed'}</span>
              </button>
              <button class="face-chip-x" title="Not {f.person!.name ?? 'this person'}" onclick={() => notThisPerson(f)}>
                <Icon name="close" size={16} />
              </button>
            </span>
          {/each}
        </div>
      {/if}
      {#if details}
        <h3 class="info-title">Details</h3>
        {#if when}
          <div class="info-row">
            <Icon name="calendar" />
            <div>
              <div>{when.date}</div>
              <div class="muted">{when.time}{details.tzOffset !== null ? ` ${formatOffset(details.tzOffset)}` : ''}</div>
            </div>
          </div>
        {/if}
        <div class="info-row">
          <Icon name={details.kind === 'video' ? 'play' : 'photo'} />
          <div>
            <div class="info-name">{details.name}</div>
            <div class="muted">
              {#if megapixels}{megapixels} MP · {details.width} × {details.height} ·{/if}
              {formatBytes(details.size)}
            </div>
            <div class="muted">
              {[fileType, details.durationMs ? formatDuration(details.durationMs) : null].filter(Boolean).join(' · ')}
            </div>
          </div>
        </div>
        {#if camera}
          <div class="info-row">
            <Icon name="camera" />
            <div>
              <div>{camera}</div>
              {#if info?.lens}<div class="muted">{info.lens}</div>{/if}
            </div>
          </div>
        {/if}
        {#if info?.software}
          <div class="info-row">
            <Icon name="edit" />
            <div>{info.software}</div>
          </div>
        {/if}
        {#if exposure || info?.flash != null}
          <div class="info-row">
            <Icon name="tune" />
            <div>
              {#if exposure}<div>{exposure}</div>{/if}
              {#if info?.flash != null}<div class="muted">{info.flash ? 'Flash fired' : 'No flash'}</div>{/if}
            </div>
          </div>
        {/if}
        {#if videoLine || info?.audioCodec}
          <div class="info-row">
            <Icon name="videocam" />
            <div>
              {#if videoLine}<div>{videoLine}</div>{/if}
              {#if info?.audioCodec}<div class="muted">Audio: {codecLabel(info.audioCodec)}</div>{/if}
            </div>
          </div>
        {/if}
        {#if details.lat !== null && details.lon !== null}
          <div class="info-row">
            <Icon name="explore" />
            <div>
              {#if place}<div>{place}</div>{/if}
              <div class:muted={place}>
                {details.lat.toFixed(5)}, {details.lon.toFixed(5)}{info?.altitude != null
                  ? ` · ${Math.round(info.altitude)} m`
                  : ''}
              </div>
            </div>
          </div>
        {/if}
        <div class="info-row">
          <Icon name="folder" />
          <button class="link" onclick={run(act.revealInExplorer)}>{details.path}</button>
        </div>
        <div class="info-row">
          <Icon name="albums" />
          <div>
            {#if albums.length === 0}
              <span class="muted">Not in any album</span>
            {:else}
              {#each albums as a (a.id)}
                <div><button class="link" onclick={() => goAlbum(a.id)}>{a.name}</button></div>
              {/each}
            {/if}
          </div>
        </div>
        {#if details.archived}
          <div class="info-row">
            <Icon name="archive" />
            <div>
              <div>Archived</div>
              <div class="muted">Shown only in the Archive</div>
            </div>
          </div>
        {/if}
        {#if details.error}
          <div class="info-row warn">
            <Icon name="warning" />
            <div>{details.error}</div>
          </div>
        {/if}
      {/if}
    </aside>
  {/if}

  {#if picking}
    <AlbumPicker
      ids={[item.id]}
      onclose={(added) => {
        picking = false
        if (added) loadAlbums(item.id)
      }}
    />
  {/if}
</div>
