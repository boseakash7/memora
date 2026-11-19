<script lang="ts">
  import { save as saveDialog } from '@tauri-apps/plugin-dialog'
  import { api, errorMessage, type Media } from '../lib/api'
  import { confirmDialog, dialogs } from '../lib/dialogs.svelte'
  import {
    ASPECTS,
    BLUR_STYLES,
    GROUPS,
    SCENE_LABEL,
    SLIDERS,
    baseSize,
    blankEdit,
    complete,
    cropFits,
    cropWithAspect,
    fitCrop,
    isBlank,
    mirrored,
    outputSize,
    previewBlob,
    sameEdit,
    turned,
    type Crop,
    type Edit,
    type FilterInfo,
    type Scene,
  } from '../lib/edit'
  import Icon from '../lib/Icon.svelte'
  import { toast } from '../lib/toast.svelte'
  import { titleBarContextMenu } from '../lib/window.svelte'
  import FeatureDownload from './FeatureDownload.svelte'
  import { feature, features, watchFeatures } from '../lib/features.svelte'

  let {
    media,
    onclose,
  }: {
    /** The photo, with its saved edit. */
    media: Media
    /** `saved`: the edit was saved or reverted. */
    onclose: (saved: boolean) => void
  } = $props()

  type Tab = 'suggest' | 'crop' | 'adjust' | 'filters'

  // svelte-ignore state_referenced_locally
  const initial = complete($state.snapshot(media.edit) as Partial<Edit> | undefined)
  // svelte-ignore state_referenced_locally
  const hadEdit = !!media.edit
  let edit = $state<Edit>(structuredClone(initial))
  let tab = $state<Tab>('suggest')
  let past = $state<Edit[]>([])
  let future = $state<Edit[]>([])
  /** The state the last history entry was taken from. */
  let committed = structuredClone(initial)

  // svelte-ignore state_referenced_locally
  const W = media.width ?? 1
  // svelte-ignore state_referenced_locally
  const H = media.height ?? 1
  const dirty = $derived(!sameEdit(edit, initial))

  // ---------- history ----------

  /** Record the current state as one step (after a slider is let go, a button...). */
  function commit() {
    const now = $state.snapshot(edit) as Edit
    if (sameEdit(now, committed)) return
    past = [...past.slice(-99), committed]
    future = []
    committed = structuredClone(now)
  }

  function set(next: Edit) {
    edit = next
    commit()
  }

  function undo() {
    if (past.length === 0) return
    future = [committed, ...future]
    committed = past[past.length - 1]
    past = past.slice(0, -1)
    edit = structuredClone(committed)
  }

  function redo() {
    if (future.length === 0) return
    past = [...past, committed]
    committed = future[0]
    future = future.slice(1)
    edit = structuredClone(committed)
  }

  // ---------- preview ----------

  let stageW = $state(0)
  let stageH = $state(0)
  let previewUrl = $state<string | null>(null)
  let previewError = $state('')
  let busy = $state(false)
  let comparing = $state(false)
  let originalUrl = $state<string | null>(null)

  /** What the preview shows: in the crop tab the whole turned picture (the frame is drawn on top). */
  const shown = $derived<Edit>(tab === 'crop' ? { ...edit, straighten: 0, crop: null } : edit)
  const [bw, bh] = $derived(baseSize(W, H, edit.rotate))
  const frame = $derived<[number, number]>(tab === 'crop' ? [bw, bh] : outputSize(edit, W, H))
  /** On-screen box of the preview. */
  const box = $derived.by(() => {
    const pad = tab === 'crop' ? 56 : 24
    const sw = Math.max(1, stageW - pad * 2)
    const sh = Math.max(1, stageH - pad * 2)
    const s = Math.min(sw / frame[0], sh / frame[1])
    return { w: Math.round(frame[0] * s), h: Math.round(frame[1] * s) }
  })
  const previewMax = $derived(Math.min(2048, Math.round(Math.max(box.w, box.h) * (window.devicePixelRatio || 1))))

  let inflight = false
  let queued = false
  /** A slider is being dragged: previews are made smaller, so they keep up. */
  let dragging = false
  let lastWasDraft = false

  async function renderPreview() {
    if (inflight) {
      queued = true
      return
    }
    inflight = true
    busy = true
    const want = $state.snapshot(shown) as Edit
    const draft = dragging
    try {
      const max = previewMax || 1024
      const blob = await previewBlob(media.id, want, draft ? Math.min(max, 900) : max)
      lastWasDraft = draft
      const url = URL.createObjectURL(blob)
      if (previewUrl) URL.revokeObjectURL(previewUrl)
      previewUrl = url
      previewError = ''
    } catch (e) {
      previewError = errorMessage(e)
    } finally {
      inflight = false
      busy = false
      if (queued) {
        queued = false
        renderPreview()
      }
    }
  }

  $effect(() => {
    JSON.stringify(shown)
    if (!previewMax) return
    renderPreview()
  })

  async function showOriginal(on: boolean) {
    comparing = on
    if (on && !originalUrl) {
      try {
        originalUrl = URL.createObjectURL(await previewBlob(media.id, blankEdit(), previewMax || 1024))
      } catch (e) {
        toast(errorMessage(e), { error: true })
      }
    }
  }

  $effect(() => () => {
    if (previewUrl) URL.revokeObjectURL(previewUrl)
    if (originalUrl) URL.revokeObjectURL(originalUrl)
    for (const u of Object.values(filterThumbs)) URL.revokeObjectURL(u)
  })

  // ---------- filters ----------

  let filters = $state<FilterInfo[]>([])
  let subjectModel = $state(false)
  $effect(() => watchFeatures())
  // Downloaded while the editor is open counts too.
  const canBlur = $derived(subjectModel || feature('blur')?.installed === true)
  const noScenes = $derived(!!features.list && feature('search')?.installed === false)
  /** Finding the subject runs on a graphics card (null: not known yet). */
  let blurOnGpu = $state<boolean | null>(null)
  let filterThumbs = $state<Record<string, string>>({})
  let thumbsFor = ''
  let thumbsAbort: AbortController | null = null

  api.edit
    .filters()
    .then((f) => {
      filters = f.filters
      subjectModel = f.subjects
      blurOnGpu = f.subjectsGpu
    })
    .catch((e) => toast(errorMessage(e), { error: true }))

  /** Small previews of every filter on the photo as edited so far. */
  async function makeThumbs() {
    const base = { ...($state.snapshot(edit) as Edit), filterStrength: 1, blur: 0 }
    const key = JSON.stringify({ ...base, filter: null })
    if (key === thumbsFor || filters.length === 0) return
    thumbsFor = key
    thumbsAbort?.abort()
    const abort = new AbortController()
    thumbsAbort = abort
    const ids = ['', ...filters.map((f) => f.id)]
    let next = 0
    const worker = async () => {
      while (next < ids.length && !abort.signal.aborted) {
        const id = ids[next++]
        try {
          const blob = await previewBlob(media.id, { ...base, filter: id || null }, 200, abort.signal)
          const url = URL.createObjectURL(blob)
          const old = filterThumbs[id]
          filterThumbs = { ...filterThumbs, [id]: url }
          if (old) URL.revokeObjectURL(old)
        } catch {
          // aborted or failed: the tile keeps its old picture
        }
      }
    }
    await Promise.all([worker(), worker(), worker(), worker()])
  }

  let thumbsTimer: ReturnType<typeof setTimeout> | undefined
  $effect(() => {
    if (tab !== 'filters' && tab !== 'suggest') return
    const _ = JSON.stringify({ ...edit, filter: null, filterStrength: 1, blur: 0 })
    void filters.length
    clearTimeout(thumbsTimer)
    thumbsTimer = setTimeout(makeThumbs, 350)
    return () => clearTimeout(thumbsTimer)
  })

  function pickFilter(id: string | null) {
    set({ ...edit, filter: id, filterStrength: id === edit.filter ? edit.filterStrength : 1 })
  }

  // ---------- suggestions (AI) ----------

  let scene = $state<Scene | null>(null)
  let suggested = $state<string[]>([])
  let enhancing = $state(false)
  let findingSubject = $state(false)
  /** The subject was found (or the saved edit already blurs), so the slider is shown. */
  // svelte-ignore state_referenced_locally
  let subjectReady = $state(initial.blur > 0)

  // svelte-ignore state_referenced_locally
  api.edit
    .enhance(media.id, initial)
    .then((r) => {
      scene = r.scene
      suggested = r.suggested
    })
    .catch(() => {})

  async function enhance() {
    enhancing = true
    try {
      const r = await api.edit.enhance(media.id, $state.snapshot(edit) as Edit)
      scene = r.scene
      suggested = r.suggested
      set({ ...edit, adjust: r.adjust })
      toast('Enhanced. Fine-tune it under Adjust.')
    } catch (e) {
      toast(errorMessage(e), { error: true })
    } finally {
      enhancing = false
    }
  }

  /** Find the photo's subject, then turn the blur on at a middle strength. */
  async function enableBlur() {
    if (findingSubject) return
    findingSubject = true
    try {
      await api.edit.subject(media.id)
      subjectReady = true
      set({ ...edit, blur: 50 })
      // Where it ran is known now.
      api.edit
        .filters()
        .then((f) => (blurOnGpu = f.subjectsGpu))
        .catch(() => {})
    } catch (e) {
      toast(errorMessage(e), { error: true })
    } finally {
      findingSubject = false
    }
  }

  const filterName = (id: string) => filters.find((f) => f.id === id)?.name ?? id

  // ---------- crop ----------

  /** Width / height the crop keeps; null is free. */
  let aspect = $state<number | null>(null)
  const fullCrop: Crop = { x: 0, y: 0, w: 1, h: 1 }
  const crop = $derived(edit.crop ?? fullCrop)

  function setAspect(ratio: number | null) {
    aspect = ratio === 0 ? bw / bh : ratio
    if (aspect === null) return
    const next = cropWithAspect(aspect, crop, bw, bh)
    set({ ...edit, crop: fitCrop(next, bw, bh, edit.straighten) })
  }

  /** The crop before the straighten slider started moving, so moving it back restores it. */
  let cropBeforeStraighten: Crop | null = null

  function straighten(deg: number) {
    cropBeforeStraighten ??= edit.crop ?? fullCrop
    edit.straighten = deg
    edit.crop = deg === 0 && cropBeforeStraighten === fullCrop ? null : fitCrop(cropBeforeStraighten, bw, bh, deg)
  }

  function straightenDone() {
    cropBeforeStraighten = null
    commit()
  }

  function resetCrop() {
    aspect = null
    set({ ...edit, rotate: 0, flip: false, straighten: 0, crop: null })
  }

  type Handle = 'move' | 'n' | 's' | 'e' | 'w' | 'ne' | 'nw' | 'se' | 'sw'
  let drag: { handle: Handle; x: number; y: number; start: Crop } | null = null

  function startDrag(e: PointerEvent, handle: Handle) {
    if (e.button !== 0) return
    e.stopPropagation()
    drag = { handle, x: e.clientX, y: e.clientY, start: { ...crop } }
    ;(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId)
  }

  function onDrag(e: PointerEvent) {
    if (!drag) return
    const dx = (e.clientX - drag.x) / box.w
    const dy = (e.clientY - drag.y) / box.h
    const s = drag.start
    const min = 0.04
    let next: Crop
    if (drag.handle === 'move') {
      // As far as it can go in the direction dragged.
      const at = (t: number): Crop => ({
        ...s,
        x: Math.min(Math.max(s.x + dx * t, 0), 1 - s.w),
        y: Math.min(Math.max(s.y + dy * t, 0), 1 - s.h),
      })
      let lo = 0
      let hi = 1
      if (cropFits(at(1), bw, bh, edit.straighten)) lo = 1
      else
        for (let i = 0; i < 20; i++) {
          const mid = (lo + hi) / 2
          if (cropFits(at(mid), bw, bh, edit.straighten)) lo = mid
          else hi = mid
        }
      edit.crop = at(lo)
      return
    }
    let { x, y, w, h } = s
    const hnd = drag.handle
    if (hnd.includes('w')) {
      const nx = Math.min(Math.max(s.x + dx, 0), s.x + s.w - min)
      w = s.x + s.w - nx
      x = nx
    }
    if (hnd.includes('e')) w = Math.min(Math.max(s.w + dx, min), 1 - s.x)
    if (hnd.includes('n')) {
      const ny = Math.min(Math.max(s.y + dy, 0), s.y + s.h - min)
      h = s.y + s.h - ny
      y = ny
    }
    if (hnd.includes('s')) h = Math.min(Math.max(s.h + dy, min), 1 - s.y)
    if (aspect !== null) {
      // Fractions: w * bw = aspect * h * bh.
      const k = (aspect * bh) / bw
      if (hnd === 'n' || hnd === 's') {
        const nw = h * k
        x = s.x + (s.w - nw) / 2
        w = nw
      } else {
        const nh = w / k
        if (hnd.includes('n')) y = s.y + s.h - nh
        else if (!hnd.includes('s')) y = s.y + (s.h - nh) / 2
        h = nh
      }
    }
    next = { x, y, w, h }
    if (next.x < -1e-6 || next.y < -1e-6 || next.x + next.w > 1 + 1e-6 || next.y + next.h > 1 + 1e-6) return
    if (!cropFits(next, bw, bh, edit.straighten)) return
    edit.crop = next
  }

  function endDrag() {
    if (!drag) return
    drag = null
    const c = edit.crop
    if (c && c.x < 1e-4 && c.y < 1e-4 && c.w > 0.9999 && c.h > 0.9999) edit.crop = null
    commit()
  }

  // ---------- save ----------

  let saving = $state(false)

  async function save() {
    if (saving) return
    saving = true
    try {
      const e = $state.snapshot(edit) as Edit
      await api.edit.save(media.id, isBlank(e) ? null : e)
      toast(isBlank(e) ? 'Back to the original' : 'Saved. The original file is unchanged.')
      onclose(true)
    } catch (e) {
      toast(errorMessage(e), { error: true })
    } finally {
      saving = false
    }
  }

  async function revert() {
    const ok = await confirmDialog({
      title: 'Go back to the original?',
      message: 'Your edits to this photo are removed. The file itself was never changed.',
      confirmLabel: 'Revert',
    })
    if (!ok) return
    try {
      await api.edit.save(media.id, null)
      toast('Back to the original')
      onclose(true)
    } catch (e) {
      toast(errorMessage(e), { error: true })
    }
  }

  async function saveCopy() {
    const dot = media.path.lastIndexOf('.')
    const stem = dot > media.path.lastIndexOf('\\') ? media.path.slice(0, dot) : media.path
    const path = await saveDialog({
      title: 'Save a copy',
      defaultPath: `${stem}-edited.jpg`,
      filters: [{ name: 'JPEG picture', extensions: ['jpg'] }],
    })
    if (!path) return
    const t = toast('Saving a copy…', { persist: true })
    try {
      const r = await api.edit.export(media.id, path, $state.snapshot(edit) as Edit)
      t.dismiss()
      toast(`Saved a copy (${r.width} × ${r.height})`)
    } catch (e) {
      t.dismiss()
      toast(errorMessage(e), { error: true })
    }
  }

  async function close() {
    if (dirty) {
      const ok = await confirmDialog({
        title: 'Discard your changes?',
        message: 'The edits you made since opening the editor are not saved.',
        confirmLabel: 'Discard',
        danger: true,
      })
      if (!ok) return
    }
    onclose(false)
  }

  function onkeydown(e: KeyboardEvent) {
    if (dialogs.current) return
    const target = e.target as HTMLElement
    const typing = target.tagName === 'INPUT' && (target as HTMLInputElement).type !== 'range'
    if (e.key === 'Escape') close()
    else if (e.ctrlKey && e.key.toLowerCase() === 'z' && !e.shiftKey) undo()
    else if (e.ctrlKey && (e.key.toLowerCase() === 'y' || (e.shiftKey && e.key.toLowerCase() === 'z'))) redo()
    else if (e.ctrlKey && e.key.toLowerCase() === 's') save()
    else if (e.key === '\\' && !typing) {
      if (!e.repeat) showOriginal(true)
    } else return
    e.preventDefault()
    e.stopPropagation()
  }

  function onkeyup(e: KeyboardEvent) {
    if (e.key === '\\') showOriginal(false)
  }

  const fmt = (v: number) => (v > 0 ? `+${Math.round(v)}` : `${Math.round(v)}`)
</script>

<svelte:window
  onkeydowncapture={onkeydown}
  {onkeyup}
  onpointerup={() => {
    if (!dragging) return
    dragging = false
    // The last preview was a quick one: make it sharp.
    if (lastWasDraft) renderPreview()
  }}
/>

<div class="editor" role="dialog" aria-modal="true" aria-label="Edit photo">
  <div class="editor-main">
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="viewer-bar editor-bar" data-tauri-drag-region="deep" oncontextmenu={titleBarContextMenu}>
      <button class="icon-btn" title="Close (Esc)" onclick={close}><Icon name="close" /></button>
      <span class="editor-title">Edit</span>
      <span class="viewer-spacer"></span>
      <button class="icon-btn" title="Undo (Ctrl+Z)" disabled={past.length === 0} onclick={undo}><Icon name="replay" /></button>
      <button class="icon-btn flip-x" title="Redo (Ctrl+Y)" disabled={future.length === 0} onclick={redo}>
        <Icon name="replay" />
      </button>
      <button
        class="icon-btn"
        class:active={comparing}
        title="Hold to see the original (\)"
        onpointerdown={() => showOriginal(true)}
        onpointerup={() => showOriginal(false)}
        onpointerleave={() => comparing && showOriginal(false)}
      >
        <Icon name="visibility" />
      </button>
      {#if hadEdit}
        <button class="btn viewer-btn" onclick={revert} title="Remove all edits">Revert</button>
      {/if}
      <button class="btn viewer-btn" onclick={saveCopy} title="Save the edited photo as a new JPEG file">
        <Icon name="download" size={20} /> Save copy
      </button>
      <button class="btn primary" disabled={saving || (!dirty && !hadEdit)} onclick={save}>Save</button>
    </div>

    <div class="editor-stage" bind:clientWidth={stageW} bind:clientHeight={stageH}>
      <div class="editor-frame" style:width="{box.w}px" style:height="{box.h}px">
        {#if previewUrl}
          <img
            class="editor-img"
            src={comparing && originalUrl && tab !== 'crop' ? originalUrl : previewUrl}
            alt=""
            draggable="false"
            style:transform={tab === 'crop' ? `rotate(${edit.straighten}deg)` : ''}
          />
        {/if}
        {#if tab === 'crop'}
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div
            class="crop-box"
            style:left="{crop.x * 100}%"
            style:top="{crop.y * 100}%"
            style:width="{crop.w * 100}%"
            style:height="{crop.h * 100}%"
            onpointerdown={(e) => startDrag(e, 'move')}
            onpointermove={onDrag}
            onpointerup={endDrag}
            onpointercancel={endDrag}
          >
            <span class="crop-grid"></span>
            {#each ['n', 's', 'e', 'w', 'ne', 'nw', 'se', 'sw'] as const as h (h)}
              <span
                class="crop-handle {h}"
                onpointerdown={(e) => startDrag(e, h)}
                onpointermove={onDrag}
                onpointerup={endDrag}
                onpointercancel={endDrag}
              ></span>
            {/each}
          </div>
        {/if}
      </div>
      {#if busy || findingSubject}
        <div class="editor-busy" aria-hidden="true"></div>
      {/if}
      {#if comparing}
        <div class="editor-badge">Original</div>
      {/if}
      {#if previewError}
        <div class="viewer-error">{previewError}</div>
      {/if}
    </div>
  </div>

  <aside class="editor-panel" onpointerdowncapture={(e) => (dragging = (e.target as HTMLElement).tagName === 'INPUT')}>
    <div class="editor-tabs" role="tablist">
      {#each [['suggest', 'Suggestions'], ['crop', 'Crop'], ['adjust', 'Adjust'], ['filters', 'Filters']] as const as [id, label] (id)}
        <button role="tab" aria-selected={tab === id} class:active={tab === id} onclick={() => (tab = id)}>{label}</button>
      {/each}
    </div>

    <div class="editor-body">
      {#if tab === 'suggest'}
        <section class="editor-section">
          <button class="editor-action" disabled={enhancing} onclick={enhance}>
            <Icon name="tune" />
            <span>
              <strong>Enhance</strong>
              <small>Balances light and colour for {scene ? SCENE_LABEL[scene] : 'this photo'}</small>
            </span>
          </button>
        </section>

        {#if noScenes}
          <p class="muted small editor-note">
            Download “Search by what’s in a photo” in Settings › AI features for filters picked to suit each photo.
          </p>
        {/if}

        {#if suggested.length > 0 && filters.length > 0}
          <section class="editor-section">
            <h3>Suggested filters{scene ? ` for ${SCENE_LABEL[scene]}` : ''}</h3>
            <div class="filter-grid">
              {#each suggested as id (id)}
                <button class="filter-tile" class:active={edit.filter === id} onclick={() => pickFilter(id)} title={filters.find((f) => f.id === id)?.about}>
                  {#if filterThumbs[id]}<img src={filterThumbs[id]} alt="" draggable="false" />{:else}<span class="filter-blank"></span>{/if}
                  <span>{filterName(id)}</span>
                </button>
              {/each}
            </div>
          </section>
        {/if}

        <section class="editor-section">
          <h3>Background blur</h3>
          {#if canBlur}
            {#if subjectReady}
              <div class="chip-row blur-styles" role="radiogroup" aria-label="Blur style">
                {#each BLUR_STYLES as b (b.id)}
                  <button
                    class="chip"
                    role="radio"
                    aria-checked={edit.blurStyle === b.id}
                    class:active={edit.blurStyle === b.id}
                    title={b.about}
                    onclick={() => set({ ...edit, blurStyle: b.id, blur: edit.blur || 50 })}>{b.label}</button
                  >
                {/each}
              </div>
              <p class="muted small">{BLUR_STYLES.find((b) => b.id === edit.blurStyle)?.about}</p>
              <label class="editor-slider">
                <span class="editor-slider-head">
                  <span>Blur</span>
                  <span class="muted">{Math.round(edit.blur)}</span>
                </span>
                <input
                  type="range"
                  min="0"
                  max="100"
                  step="1"
                  value={edit.blur}
                  oninput={(e) => (edit.blur = +e.currentTarget.value)}
                  onchange={commit}
                />
              </label>
            {:else}
              <p class="muted small">Finds the main subject of the photo and softly blurs what's behind it.</p>
              <button class="btn viewer-btn blur-btn" disabled={findingSubject} onclick={enableBlur}>
                {#if findingSubject}
                  <span class="spinner" aria-hidden="true"></span> Finding the subject…
                {:else}
                  <Icon name="people" size={20} /> Blur the background
                {/if}
              </button>
            {/if}
            {#if blurOnGpu === false}
              <p class="muted small editor-note">
                For better performance a dedicated graphics card is required. Without one, finding the subject takes a few
                seconds per photo.
              </p>
            {/if}
          {:else}
            <p class="muted small">Finds the main subject of the photo and softly blurs what's behind it. Needs a one-time download.</p>
            <FeatureDownload of="blur" compact />
          {/if}
        </section>
      {:else if tab === 'crop'}
        <section class="editor-section">
          <div class="editor-row">
            <button class="btn viewer-btn" onclick={() => set(turned(edit))}><Icon name="refresh" size={20} /> Rotate</button>
            <button class="btn viewer-btn" onclick={() => set(mirrored(edit))}><Icon name="openInFull" size={20} /> Flip</button>
            <span class="viewer-spacer"></span>
            <button class="btn viewer-btn" onclick={resetCrop}>Reset</button>
          </div>
        </section>
        <section class="editor-section">
          <label class="editor-slider">
            <span class="editor-slider-head">
              <span>Straighten</span>
              <span class="muted">{edit.straighten.toFixed(1)}°</span>
            </span>
            <input
              type="range"
              min="-45"
              max="45"
              step="0.1"
              value={edit.straighten}
              oninput={(e) => straighten(+e.currentTarget.value)}
              onchange={straightenDone}
              ondblclick={() => {
                straighten(0)
                straightenDone()
              }}
            />
          </label>
        </section>
        <section class="editor-section">
          <h3>Aspect ratio</h3>
          <div class="chip-row">
            {#each ASPECTS as a (a.label)}
              <button
                class="chip"
                class:active={a.ratio === null ? aspect === null : a.ratio === 0 ? aspect !== null && Math.abs(aspect - bw / bh) < 1e-6 : aspect === a.ratio}
                onclick={() => setAspect(a.ratio)}>{a.label}</button
              >
            {/each}
          </div>
          <p class="muted small">Drag the frame or its corners. The rest of the photo stays in the file.</p>
        </section>
      {:else if tab === 'adjust'}
        {#each SLIDERS as group (group.title)}
          <section class="editor-section">
            <h3>{group.title}</h3>
            {#each group.items as s (s.key)}
              <label class="editor-slider" class:changed={edit.adjust[s.key] !== 0}>
                <span class="editor-slider-head">
                  <span>{s.label}</span>
                  <span class="muted">{fmt(edit.adjust[s.key])}</span>
                </span>
                <input
                  type="range"
                  min={s.min}
                  max={s.max}
                  step="1"
                  value={edit.adjust[s.key]}
                  class:centered={s.min < 0}
                  oninput={(e) => (edit.adjust[s.key] = +e.currentTarget.value)}
                  onchange={commit}
                  ondblclick={() => {
                    edit.adjust[s.key] = 0
                    commit()
                  }}
                  title="Double-click to reset"
                />
              </label>
            {/each}
          </section>
        {/each}
        <section class="editor-section">
          <button class="btn viewer-btn" onclick={() => set({ ...edit, adjust: blankEdit().adjust })}>Reset adjustments</button>
        </section>
      {:else}
        {#if edit.filter}
          <section class="editor-section">
            <label class="editor-slider">
              <span class="editor-slider-head">
                <span>{filterName(edit.filter)}</span>
                <span class="muted">{Math.round(edit.filterStrength * 100)}%</span>
              </span>
              <input
                type="range"
                min="0"
                max="100"
                step="1"
                value={Math.round(edit.filterStrength * 100)}
                oninput={(e) => (edit.filterStrength = +e.currentTarget.value / 100)}
                onchange={commit}
              />
            </label>
          </section>
        {/if}
        <section class="editor-section">
          <div class="filter-grid">
            <button class="filter-tile" class:active={!edit.filter} onclick={() => pickFilter(null)}>
              {#if filterThumbs['']}<img src={filterThumbs['']} alt="" draggable="false" />{:else}<span class="filter-blank"></span>{/if}
              <span>None</span>
            </button>
          </div>
        </section>
        {#each GROUPS as g (g.id)}
          {@const list = filters.filter((f) => f.group === g.id)}
          {#if list.length > 0}
            <section class="editor-section">
              <h3>{g.label}</h3>
              <div class="filter-grid">
                {#each list as f (f.id)}
                  <button class="filter-tile" class:active={edit.filter === f.id} onclick={() => pickFilter(f.id)} title={f.about}>
                    {#if filterThumbs[f.id]}<img src={filterThumbs[f.id]} alt="" draggable="false" />{:else}<span class="filter-blank"></span>{/if}
                    <span>{f.name}</span>
                  </button>
                {/each}
              </div>
            </section>
          {/if}
        {/each}
      {/if}
    </div>
  </aside>
</div>
