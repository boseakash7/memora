<script lang="ts" module>
  // Playback choices carry over from one video to the next, and volume across launches.
  type Prefs = { volume: number; muted: boolean; rate: number; loop: boolean; remaining: boolean }
  const PREFS_KEY = 'memora.video'
  const prefs: Prefs = { volume: 1, muted: false, rate: 1, loop: false, remaining: false }
  try {
    const saved = JSON.parse(localStorage.getItem(PREFS_KEY) ?? '{}')
    if (typeof saved.volume === 'number') prefs.volume = Math.min(1, Math.max(0, saved.volume))
    if (typeof saved.muted === 'boolean') prefs.muted = saved.muted
  } catch {
    // Nothing saved yet.
  }
  function savePrefs() {
    try {
      localStorage.setItem(PREFS_KEY, JSON.stringify({ volume: prefs.volume, muted: prefs.muted }))
    } catch {
      // Storage full or blocked; the choice still holds for this session.
    }
  }

  const SPEEDS = [0.25, 0.5, 0.75, 1, 1.25, 1.5, 1.75, 2, 3]
  /** One frame for `,` and `.`; the frame rate isn't known here, and most phone videos are 30 fps. */
  const FRAME = 1 / 30
  const HIDE_AFTER_MS = 2500
</script>

<script lang="ts">
  import { onDestroy, onMount, tick } from 'svelte'
  import * as act from '../lib/actions'
  import { loadStoryboard, mediaUrl, streamUrl, type Storyboard, type TimelineItem } from '../lib/api'
  import { contextMenu } from '../lib/contextMenu.svelte'
  import Icon from '../lib/Icon.svelte'
  import { icons, type IconName } from '../lib/icons'
  import { appWindow, setFullscreen } from '../lib/window.svelte'

  let {
    item,
    box,
    keys = true,
    idle = $bindable(false),
    onopenexternal,
  }: {
    item: TimelineItem
    /** On-screen size of the video, fitted to the stage. */
    box: { w: number; h: number }
    /** Keyboard shortcuts on (off while a dialog or picker is open). */
    keys?: boolean
    /** Controls hidden while playing and the mouse is still; the viewer hides its bar too. */
    idle?: boolean
    onopenexternal: () => void
  } = $props()

  let video = $state<HTMLVideoElement>()
  /** `converted`: the webview can't play the original, so the PC converts it while it plays. */
  let mode = $state<'original' | 'converted' | 'failed'>('original')
  /** Converted: the file time the current stream starts at (its own clock starts at 0). */
  let offset = $state(0)
  /** Converted: bumped to start a new stream even when `offset` is unchanged (loop, replay). */
  let generation = $state(0)
  /** Converted: a stream is starting at this time; shown on the bar meanwhile. */
  let pending = $state<number | null>(null)
  let resumeAfterLoad = true
  /** The thumbnail until the first frame shows; later loads (converted jumps) keep it off, so it doesn't flash. */
  // The viewer makes a new player for each item, so reading `item` once is enough here and below.
  // svelte-ignore state_referenced_locally
  let poster = $state<string | undefined>(mediaUrl(item.id, 'thumb', item.m))

  let paused = $state(true)
  let ended = $state(false)
  let waiting = $state(false)
  let time = $state(0)
  let mediaDuration = $state(NaN)
  let volume = $state(prefs.volume)
  let muted = $state(prefs.muted)
  let rate = $state(prefs.rate)
  let loop = $state(prefs.loop)
  let remaining = $state(prefs.remaining)
  let buffered = $state<[number, number]>([0, 0])

  let menuOpen = $state(false)
  let keysOpen = $state(false)
  let story = $state<Storyboard | null>(null)

  const src = $derived(
    mode === 'converted'
      ? `${streamUrl(item.id, offset)}${generation ? `&r=${generation}` : ''}`
      : mediaUrl(item.id, 'original'),
  )
  // svelte-ignore state_referenced_locally
  const knownDuration = item.d ? item.d / 1000 : 0
  const duration = $derived.by(() => {
    const own = Number.isFinite(mediaDuration) && mediaDuration > 0 ? mediaDuration : 0
    if (mode === 'converted') return knownDuration || (own ? offset + own : 0)
    return own || knownDuration
  })

  // ---------- seek bar state ----------
  let seekEl = $state<HTMLDivElement>()
  let seekW = $state(0)
  /** Pointer position over the seek bar, in px from its left edge. */
  let hoverX = $state<number | null>(null)
  let scrub = $state<{ t: number; resume: boolean } | null>(null)

  const current = $derived(scrub ? scrub.t : (pending ?? (mode === 'converted' ? offset : 0) + time))
  const fraction = (t: number) => (duration > 0 ? Math.min(1, Math.max(0, t / duration)) : 0)
  const hoverT = $derived(hoverX === null || !seekW ? 0 : (Math.min(1, Math.max(0, hoverX / seekW)) * duration))

  function fmt(t: number) {
    t = Math.max(0, Math.floor(Number.isFinite(t) ? t : 0))
    const h = Math.floor(t / 3600)
    const m = Math.floor((t % 3600) / 60)
    const s = String(t % 60).padStart(2, '0')
    return h ? `${h}:${String(m).padStart(2, '0')}:${s}` : `${m}:${s}`
  }

  // ---------- storyboard preview ----------
  // svelte-ignore state_referenced_locally
  const aspect = item.w && item.h ? item.w / item.h : 16 / 9
  const PREVIEW = 176
  const previewW = aspect >= 1 ? PREVIEW : Math.round(PREVIEW * aspect)
  const previewH = aspect >= 1 ? Math.round(PREVIEW / aspect) : PREVIEW
  const storyStyle = $derived.by(() => {
    if (!story) return ''
    const i = Math.min(story.frames - 1, Math.floor(hoverT / story.interval))
    const col = i % story.cols
    const row = Math.floor(i / story.cols)
    return (
      `background-image: url(${story.url}); ` +
      `background-size: ${story.cols * previewW}px ${story.rows * previewH}px; ` +
      `background-position: -${col * previewW}px -${row * previewH}px`
    )
  })
  const previewLeft = $derived(
    hoverX === null ? 0 : Math.min(Math.max(hoverX, previewW / 2 - 8), seekW - previewW / 2 + 8) - previewW / 2,
  )

  const storyAbort = new AbortController()
  // Asked for a moment after opening, so it doesn't compete with starting playback.
  const storyTimer = setTimeout(() => {
    loadStoryboard(item.id, item.m, storyAbort.signal)
      .then((s) => (story = s))
      .catch(() => {
        // No preview; the bar still shows the time.
      })
  }, 600)

  // ---------- playback ----------
  onMount(() => {
    start()
    poke()
    let raf = requestAnimationFrame(function tick() {
      if (video) {
        time = video.currentTime
        buffered = bufferedRange(video)
      }
      raf = requestAnimationFrame(tick)
    })
    return () => cancelAnimationFrame(raf)
  })

  onDestroy(() => {
    clearTimeout(storyTimer)
    clearTimeout(idleTimer)
    clearTimeout(clickTimer)
    clearTimeout(osdTimer)
    storyAbort.abort()
    if (story) URL.revokeObjectURL(story.url)
  })

  /** Play once loaded; if the webview refuses sound without a click, play muted. */
  function start() {
    video?.play().catch((e) => {
      if (e?.name === 'NotAllowedError' && video && !video.muted) {
        muted = true
        video.play().catch(() => {})
      }
    })
  }

  function onloaded() {
    if (!video) return
    poster = undefined
    video.defaultPlaybackRate = rate
    video.playbackRate = rate
    if (pending !== null) {
      pending = null
      if (resumeAfterLoad) start()
    }
  }

  function onerror() {
    if (mode === 'original') {
      // The webview can't decode it (e.g. HEVC without the Windows extension): convert it.
      offset = time > 0.5 ? time : 0
      pending = offset
      resumeAfterLoad = true
      mode = 'converted'
    } else if (mode === 'converted') {
      mode = 'failed'
      pending = null
    }
  }

  function onended() {
    if (mode === 'converted' && loop) restartAt(0, true)
  }

  function bufferedRange(v: HTMLVideoElement): [number, number] {
    const base = mode === 'converted' ? offset : 0
    const t = v.currentTime
    for (let i = 0; i < v.buffered.length; i++) {
      if (v.buffered.start(i) <= t + 0.5 && v.buffered.end(i) >= t) return [base + v.buffered.start(i), base + v.buffered.end(i)]
    }
    return [base + t, base + t]
  }

  function inSeekable(v: HTMLVideoElement, t: number) {
    for (let i = 0; i < v.seekable.length; i++) if (v.seekable.start(i) <= t && t <= v.seekable.end(i)) return true
    return false
  }

  /** Converted: start a new stream at `t` (FFmpeg restarts there). */
  function restartAt(t: number, resume: boolean) {
    resumeAfterLoad = resume
    pending = t
    offset = t
    generation++
  }

  function seekTo(t: number, resume?: boolean) {
    if (!video || mode === 'failed') return
    t = Math.min(Math.max(0, t), Math.max(0, duration - 0.05))
    if (mode === 'converted') {
      const local = t - offset
      const playing = pending !== null ? resumeAfterLoad : !video.paused
      if (pending === null && local >= 0 && inSeekable(video, local)) video.currentTime = local
      else restartAt(t, resume ?? playing)
    } else {
      video.currentTime = t
    }
  }

  function togglePlay() {
    if (!video || mode === 'failed') return
    if (pending !== null) {
      resumeAfterLoad = !resumeAfterLoad
      flash(resumeAfterLoad ? 'play' : 'pause')
      return
    }
    if (ended && mode === 'converted') {
      restartAt(0, true)
      flash('play')
    } else if (video.paused || ended) {
      start()
      flash('play')
    } else {
      video.pause()
      flash('pause')
    }
  }

  function skip(d: number) {
    seekTo(current + d)
    osd(d < 0 ? 'fastRewind' : 'fastForward', `${d < 0 ? '−' : '+'}${Math.abs(d)} s`)
  }

  function step(dir: -1 | 1) {
    if (pending !== null) resumeAfterLoad = false
    else video?.pause()
    seekTo(current + dir * FRAME, false)
    osd(dir < 0 ? 'fastRewind' : 'fastForward', dir < 0 ? '1 frame back' : '1 frame forward')
  }

  // ---------- volume, speed, loop ----------
  $effect(() => {
    prefs.volume = volume
    prefs.muted = muted
    prefs.rate = rate
    prefs.loop = loop
    prefs.remaining = remaining
    savePrefs()
  })

  const volumeIcon = $derived<IconName>(muted || volume === 0 ? 'volumeOff' : volume < 0.5 ? 'volumeDown' : 'volumeUp')

  function setVolume(v: number, announce = false) {
    v = Math.round(Math.min(1, Math.max(0, v)) * 100) / 100
    volume = v
    muted = v === 0
    if (announce) osd(volumeIcon, v === 0 ? 'Muted' : `Volume ${Math.round(v * 100)}%`)
  }

  function toggleMute(announce = false) {
    if (muted || volume === 0) {
      muted = false
      if (volume === 0) volume = 0.5
    } else muted = true
    if (announce) osd(volumeIcon, muted ? 'Muted' : `Volume ${Math.round(volume * 100)}%`)
  }

  function setRate(r: number, announce = false) {
    rate = r
    if (video) {
      video.defaultPlaybackRate = r
      video.playbackRate = r
    }
    if (announce) osd('speed', r === 1 ? 'Normal speed' : `${r}×`)
  }

  function stepRate(d: number) {
    const i = SPEEDS.indexOf(rate)
    setRate(SPEEDS[Math.min(SPEEDS.length - 1, Math.max(0, (i < 0 ? 3 : i) + d))], true)
  }

  function toggleLoop() {
    loop = !loop
    osd('repeat', loop ? 'Loop on' : 'Loop off')
  }

  // ---------- auto-hide ----------
  let idleTimer = 0
  let overChrome = false
  function poke() {
    idle = false
    clearTimeout(idleTimer)
    idleTimer = window.setTimeout(() => {
      const playing = pending !== null ? resumeAfterLoad : !paused && !ended
      if (playing && !menuOpen && !keysOpen && !scrub && !overChrome && !contextMenu.current) idle = true
    }, HIDE_AFTER_MS)
  }
  // Paused, the controls stay up.
  $effect(() => {
    if (paused || ended || mode === 'failed') {
      idle = false
      clearTimeout(idleTimer)
    } else poke()
  })

  function onwindowpointermove(e: PointerEvent) {
    if (e.pointerType !== 'mouse') return
    overChrome = !!(e.target as Element | null)?.closest?.('.viewer-bar, .vp-controls, .vp-menu, .info-panel')
    poke()
  }

  // ---------- clicks on the picture ----------
  let clickTimer = 0
  const onControl = (e: Event) => !!(e.target as Element).closest('.vp-controls, .vp-menu, .vp-center, .vp-message')

  function onclick(e: MouseEvent) {
    if (onControl(e) || (e as PointerEvent).pointerType === 'touch') return
    clearTimeout(clickTimer)
    clickTimer = window.setTimeout(togglePlay, 200)
  }

  function ondblclick(e: MouseEvent) {
    if (onControl(e)) return
    clearTimeout(clickTimer)
    setFullscreen(!appWindow.fullscreen)
  }

  // Touch: a tap shows or hides the controls instead of pausing.
  function onpointerup(e: PointerEvent) {
    if (e.pointerType !== 'touch' || onControl(e)) return
    if (idle) poke()
    else if (!paused) {
      clearTimeout(idleTimer)
      idle = true
    }
  }

  // ---------- feedback ----------
  let flashIcon = $state<IconName>('play')
  let flashKey = $state(0)
  function flash(icon: IconName) {
    flashIcon = icon
    flashKey++
  }

  let osdState = $state<{ icon: IconName; text: string } | null>(null)
  let osdTimer = 0
  function osd(icon: IconName, text: string) {
    osdState = { icon, text }
    clearTimeout(osdTimer)
    osdTimer = window.setTimeout(() => (osdState = null), 800)
  }

  // ---------- seek bar ----------
  const tAt = (clientX: number) => {
    const r = seekEl!.getBoundingClientRect()
    return Math.min(1, Math.max(0, (clientX - r.left) / r.width)) * duration
  }

  function onseekmove(e: PointerEvent) {
    const r = seekEl!.getBoundingClientRect()
    hoverX = Math.min(r.width, Math.max(0, e.clientX - r.left))
    if (scrub) {
      scrub.t = tAt(e.clientX)
      // Originals follow the pointer; a converted video jumps once, when let go.
      if (mode === 'original' && video) video.currentTime = scrub.t
    }
  }

  function onseekdown(e: PointerEvent) {
    if (e.button !== 0 || !video || !duration || mode === 'failed') return
    seekEl!.setPointerCapture(e.pointerId)
    const resume = pending !== null ? resumeAfterLoad : !video.paused
    if (mode === 'original') video.pause()
    scrub = { t: tAt(e.clientX), resume }
    onseekmove(e)
  }

  function onseekup(e: PointerEvent) {
    if (!scrub) return
    const { t, resume } = scrub
    scrub = null
    if (e.pointerType !== 'mouse') hoverX = null
    if (mode === 'converted') seekTo(t, resume)
    else if (resume) start()
    poke()
  }

  function onseekkey(e: KeyboardEvent) {
    if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') {
      e.preventDefault()
      e.stopPropagation() // not "previous / next item"
      skip(e.key === 'ArrowLeft' ? -5 : 5)
    }
  }

  // ---------- volume slider ----------
  let volEl = $state<HTMLDivElement>()
  let volDrag = $state(false)
  const volAt = (clientX: number) => {
    const r = volEl!.getBoundingClientRect()
    return (clientX - r.left) / r.width
  }

  function onvolkey(e: KeyboardEvent) {
    const d = e.key === 'ArrowLeft' || e.key === 'ArrowDown' ? -0.05 : e.key === 'ArrowRight' || e.key === 'ArrowUp' ? 0.05 : 0
    if (!d) return
    e.preventDefault()
    e.stopPropagation()
    setVolume((muted ? 0 : volume) + d, true)
  }

  // ---------- menu ----------
  let settingsBtn = $state<HTMLButtonElement>()
  let menuEl = $state<HTMLDivElement>()
  function openMenu(open: boolean) {
    menuOpen = open
    if (open) {
      idle = false
      tick().then(() => menuEl?.querySelector<HTMLElement>('[aria-pressed="true"]')?.focus())
    } else poke()
  }

  function onwindowpointerdown(e: PointerEvent) {
    const t = e.target as Node
    if (menuOpen && !menuEl?.contains(t) && !settingsBtn?.contains(t)) openMenu(false)
  }

  // ---------- keyboard ----------
  let spaceHandled = false
  const stop = (e: Event) => {
    e.preventDefault()
    e.stopPropagation()
  }

  // Capture phase: runs before the viewer's own keys, so Esc closes the menu first.
  function onkeydown(e: KeyboardEvent) {
    if (!keys || e.ctrlKey || e.metaKey || e.altKey || contextMenu.current) return
    if ((e.target as Element | null)?.closest?.('input, textarea, select, [contenteditable="true"]')) return
    if (keysOpen) {
      if (e.key === 'Escape' || e.key === '?') {
        stop(e)
        keysOpen = false
        poke()
      } else if (e.key !== 'Tab') e.stopPropagation()
      return
    }
    if (menuOpen) {
      if (e.key === 'Escape') {
        stop(e)
        openMenu(false)
        settingsBtn?.focus()
      } else if (e.key.startsWith('Arrow')) e.stopPropagation()
      if (e.key !== ' ' && e.key !== 'Enter') return
      if ((e.target as Element | null)?.closest?.('.vp-menu')) return
    }
    if (mode === 'failed') return
    const k = e.key.length === 1 ? e.key.toLowerCase() : e.key
    switch (k) {
      case ' ':
      case 'k':
        togglePlay()
        spaceHandled = k === ' '
        break
      case 'j':
        skip(-10)
        break
      case 'l':
        skip(10)
        break
      case ',':
        step(-1)
        break
      case '.':
        step(1)
        break
      case 'm':
        toggleMute(true)
        break
      case 'f':
        setFullscreen(!appWindow.fullscreen)
        break
      case 'ArrowUp':
        setVolume((muted ? 0 : volume) + 0.1, true)
        break
      case 'ArrowDown':
        setVolume((muted ? 0 : volume) - 0.1, true)
        break
      case 'Home':
        seekTo(0)
        osd('fastRewind', 'Start')
        break
      case 'End':
        seekTo(duration)
        osd('fastForward', 'End')
        break
      case '[':
        stepRate(-1)
        break
      case ']':
        stepRate(1)
        break
      case '?':
        keysOpen = true
        idle = false
        break
      default:
        if (/^[0-9]$/.test(k) && duration) {
          seekTo((duration * Number(k)) / 10)
          osd('fastForward', `${Number(k) * 10}%`)
          break
        }
        return
    }
    e.preventDefault()
    poke()
  }

  // A focused button would otherwise also "click" on Space.
  function onkeyup(e: KeyboardEvent) {
    if (e.key === ' ' && spaceHandled) {
      e.preventDefault()
      spaceHandled = false
    }
  }

  $effect(() => {
    window.addEventListener('keydown', onkeydown, true)
    window.addEventListener('keyup', onkeyup, true)
    return () => {
      window.removeEventListener('keydown', onkeydown, true)
      window.removeEventListener('keyup', onkeyup, true)
    }
  })

  const playIcon = $derived<IconName>(ended && !loop ? 'replay' : (pending !== null ? resumeAfterLoad : !paused) ? 'pause' : 'play')
  const shownTime = $derived(remaining ? `−${fmt(duration - current)}` : fmt(current))
</script>

<svelte:window onpointermove={onwindowpointermove} onpointerdown={onwindowpointerdown} />

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="vp" {onclick} {ondblclick} {onpointerup}>
  {#if mode === 'failed'}
    <div class="viewer-message vp-message">
      <Icon name="brokenImage" size={48} />
      <p>This video's format can't be played here.</p>
      <button class="btn" onclick={onopenexternal}>
        <Icon name="openExternal" size={20} /> Open in default player
      </button>
    </div>
  {:else}
    <!-- svelte-ignore a11y_media_has_caption -->
    <video
      bind:this={video}
      class="vp-video"
      style:width="{box.w}px"
      style:height="{box.h}px"
      {src}
      {poster}
      preload="auto"
      loop={loop && mode === 'original'}
      bind:paused
      bind:ended
      bind:volume
      bind:muted
      bind:duration={mediaDuration}
      onloadeddata={onloaded}
      onwaiting={() => (waiting = true)}
      onplaying={() => (waiting = false)}
      oncanplay={() => (waiting = false)}
      onseeked={() => (waiting = false)}
      {onerror}
      {onended}
    ></video>

    {#if waiting || pending !== null}
      <div class="vp-spinner"></div>
    {/if}
    {#key flashKey}
      {#if flashKey}
        <div class="vp-flash"><Icon name={flashIcon} size={40} /></div>
      {/if}
    {/key}
    {#if ended && !loop && pending === null}
      <button class="vp-center" title="Replay" onclick={togglePlay}><Icon name="replay" size={40} /></button>
    {/if}
    {#if osdState}
      <div class="vp-osd"><Icon name={osdState.icon} size={20} /><span>{osdState.text}</span></div>
    {/if}

    <div class="vp-controls">
      <div
        class="vp-seek"
        class:dragging={scrub}
        bind:this={seekEl}
        bind:clientWidth={seekW}
        role="slider"
        tabindex="0"
        aria-label="Seek"
        aria-valuemin={0}
        aria-valuemax={Math.round(duration)}
        aria-valuenow={Math.round(current)}
        aria-valuetext="{fmt(current)} of {fmt(duration)}"
        onpointermove={onseekmove}
        onpointerdown={onseekdown}
        onpointerup={onseekup}
        onpointercancel={onseekup}
        onpointerleave={() => {
          if (!scrub) hoverX = null
        }}
        onkeydown={onseekkey}
      >
        <div class="vp-track">
          <div
            class="vp-buffer"
            style:left="{fraction(buffered[0]) * 100}%"
            style:width="{(fraction(buffered[1]) - fraction(buffered[0])) * 100}%"
          ></div>
          {#if hoverX !== null && !scrub}
            <div class="vp-hover" style:width="{fraction(hoverT) * 100}%"></div>
          {/if}
          <div class="vp-played" style:width="{fraction(current) * 100}%"></div>
          <div class="vp-thumb" style:left="{fraction(current) * 100}%"></div>
        </div>
        {#if hoverX !== null && duration}
          <div class="vp-preview" style:left="{previewLeft}px" style:width="{previewW}px">
            {#if story}
              <div class="vp-preview-img" style:width="{previewW}px" style:height="{previewH}px" style={storyStyle}></div>
            {/if}
            <span class="vp-preview-time">{fmt(scrub ? scrub.t : hoverT)}</span>
          </div>
        {/if}
      </div>

      <div class="vp-row">
        <button class="icon-btn" title={playIcon === 'pause' ? 'Pause (K)' : playIcon === 'replay' ? 'Replay' : 'Play (K)'} onclick={togglePlay}>
          <Icon name={playIcon} size={28} />
        </button>
        <button class="icon-btn vp-skip" title="Back 10 seconds (J)" onclick={() => skip(-10)}>
          <svg width="26" height="26" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
            <path d={icons.replay} /><text x="12" y="16.2" text-anchor="middle">10</text>
          </svg>
        </button>
        <button class="icon-btn vp-skip" title="Forward 10 seconds (L)" onclick={() => skip(10)}>
          <svg width="26" height="26" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
            <path d={icons.forwardArrow} /><text x="12" y="16.2" text-anchor="middle">10</text>
          </svg>
        </button>
        <div class="vp-volume" class:dragging={volDrag}>
          <button class="icon-btn" title={muted || volume === 0 ? 'Unmute (M)' : 'Mute (M)'} onclick={() => toggleMute()}>
            <Icon name={volumeIcon} />
          </button>
          <div class="vp-volume-slider">
            <div
              class="vp-volume-hit"
              role="slider"
              tabindex="0"
              aria-label="Volume"
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={Math.round((muted ? 0 : volume) * 100)}
              onpointerdown={(e) => {
                volDrag = true
                ;(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId)
                setVolume(volAt(e.clientX))
              }}
              onpointermove={(e) => volDrag && setVolume(volAt(e.clientX))}
              onpointerup={() => (volDrag = false)}
              onpointercancel={() => (volDrag = false)}
              onkeydown={onvolkey}
            >
              <div class="vp-volume-track" bind:this={volEl}>
                <div class="vp-volume-fill" style:width="{(muted ? 0 : volume) * 100}%"></div>
                <div class="vp-volume-knob" style:left="{(muted ? 0 : volume) * 100}%"></div>
              </div>
            </div>
          </div>
        </div>
        <button class="vp-time" title={remaining ? 'Show time played' : 'Show time left'} onclick={() => (remaining = !remaining)}>
          {shownTime} <span class="muted">/ {fmt(duration)}</span>
        </button>
        <span class="vp-spacer"></span>
        {#if mode === 'converted'}
          <span
            class="vp-chip"
            title="This format can't play here directly, so the PC converts it while it plays. Jumping ahead takes a moment."
          >
            <Icon name="sync" size={14} /> Converted
          </span>
        {/if}
        {#if rate !== 1}
          <button class="vp-rate" title="Playback speed" onclick={() => openMenu(true)}>{rate}×</button>
        {/if}
        <button class="icon-btn" title="Copy this frame" onclick={() => act.copyVideoFrame(item.id, current)}>
          <Icon name="contentCopy" size={22} />
        </button>
        <button
          class="icon-btn"
          title="Playback settings"
          aria-haspopup="menu"
          aria-expanded={menuOpen}
          bind:this={settingsBtn}
          onclick={() => openMenu(!menuOpen)}
        >
          <Icon name="settings" />
        </button>
        <button
          class="icon-btn"
          title={appWindow.fullscreen ? 'Exit full screen (F)' : 'Full screen (F)'}
          onclick={() => setFullscreen(!appWindow.fullscreen)}
        >
          <Icon name={appWindow.fullscreen ? 'fullscreenExit' : 'fullscreen'} size={26} />
        </button>
      </div>
    </div>

    {#if menuOpen}
      <div class="vp-menu" role="menu" bind:this={menuEl}>
        <div class="vp-menu-label">Playback speed</div>
        <div class="vp-speeds">
          {#each SPEEDS as s (s)}
            <button class="vp-speed" aria-pressed={rate === s} onclick={() => setRate(s)}>{s === 1 ? 'Normal' : `${s}×`}</button>
          {/each}
        </div>
        <div class="vp-menu-sep"></div>
        <button class="vp-menu-item" role="menuitemcheckbox" aria-checked={loop} onclick={toggleLoop}>
          <Icon name="repeat" size={20} /><span class="vp-grow">Loop</span><span class="vp-switch"></span>
        </button>
        <button
          class="vp-menu-item"
          role="menuitem"
          onclick={() => {
            openMenu(false)
            keysOpen = true
          }}
        >
          <Icon name="keyboard" size={20} /><span class="vp-grow">Keyboard shortcuts</span><kbd>?</kbd>
        </button>
      </div>
    {/if}
  {/if}
</div>

{#if keysOpen}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="vp-keys-backdrop" onclick={(e) => e.target === e.currentTarget && (keysOpen = false)}>
    <div class="vp-keys" role="dialog" aria-modal="true" aria-labelledby="vp-keys-title">
      <div class="vp-keys-head">
        <h2 id="vp-keys-title">Keyboard shortcuts</h2>
        <button class="icon-btn" title="Close (Esc)" onclick={() => (keysOpen = false)}><Icon name="close" /></button>
      </div>
      <div class="vp-keys-grid">
        <div><span>Play / pause</span><span><kbd>Space</kbd><kbd>K</kbd></span></div>
        <div><span>Back / forward 10 s</span><span><kbd>J</kbd><kbd>L</kbd></span></div>
        <div><span>One frame back / forward</span><span><kbd>,</kbd><kbd>.</kbd></span></div>
        <div><span>Jump to 0–90%</span><span><kbd>0</kbd>–<kbd>9</kbd></span></div>
        <div><span>Start / end</span><span><kbd>Home</kbd><kbd>End</kbd></span></div>
        <div><span>Volume up / down</span><span><kbd>↑</kbd><kbd>↓</kbd></span></div>
        <div><span>Mute</span><span><kbd>M</kbd></span></div>
        <div><span>Slower / faster</span><span><kbd>[</kbd><kbd>]</kbd></span></div>
        <div><span>Full screen</span><span><kbd>F</kbd></span></div>
        <div><span>Previous / next item</span><span><kbd>←</kbd><kbd>→</kbd></span></div>
        <div><span>Skip 5 s (seek bar focused)</span><span><kbd>←</kbd><kbd>→</kbd></span></div>
        <div><span>Close viewer</span><span><kbd>Esc</kbd></span></div>
      </div>
      <p>← and → move between photos and videos, as for photos. Click the seek bar (or Tab to it) and they skip 5 seconds instead.</p>
    </div>
  </div>
{/if}
