<script lang="ts">
  import { open } from '@tauri-apps/plugin-dialog'
  import { api, errorMessage, type Feature, type Folder } from '../lib/api'
  import Icon from '../lib/Icon.svelte'
  import { askKeepInfo } from '../lib/folders'
  import {
    FEATURE_INFO,
    FEATURES,
    downloadFeature,
    features,
    mb,
    pauseFeature,
    watchFeatures,
  } from '../lib/features.svelte'
  import { library } from '../lib/library.svelte'
  import { titleBarContextMenu } from '../lib/window.svelte'

  /**
   * First start: what Memora is, which AI features to download, and the first folder.
   * `ondone` once it is finished or skipped (the setting is saved here).
   */
  let { ondone }: { ondone: () => void } = $props()

  let step = $state<1 | 2 | 3 | 4>(1)
  let chosen = $state<Record<Feature, boolean>>({ faces: true, search: true, blur: true })
  /** The features whose downloads were started from step 2. */
  let mine = $state<Feature[]>([])
  const started = $derived(mine.length > 0)
  let skipped = $state(false)
  let folders = $state<Folder[]>([])
  let adding = $state(false)
  let folderError = $state('')
  let leaving = $state(false)

  $effect(() => watchFeatures())

  $effect(() => {
    api.folders
      .list()
      .then((f) => (folders = f))
      .catch(() => {})
  })

  const list = $derived(features.list ?? [])
  /** Everything came with the installer: nothing to choose. */
  const bundled = $derived(list.length > 0 && list.every((f) => f.installed))
  const toGet = $derived(list.filter((f) => !f.installed && chosen[f.feature]))
  const total = $derived(toGet.reduce((n, f) => n + f.size, 0))
  const blurNote = $derived.by(() => {
    const b = list.find((f) => f.feature === 'blur')
    if (!b || b.installed) return ''
    return b.full
      ? 'Best-quality version, because this PC has a graphics card.'
      : 'Lighter version, picked because this PC has no graphics card it can use.'
  })

  /** The downloads started here, for the progress card in step 3. */
  const jobs = $derived(list.filter((f) => mine.includes(f.feature)))
  const dlGot = $derived(jobs.reduce((n, f) => n + (f.installed ? f.size : (f.job?.got ?? 0)), 0))
  const dlTotal = $derived(jobs.reduce((n, f) => n + (f.installed ? f.size : (f.job?.total ?? f.size)), 0))
  const dlDone = $derived(jobs.length > 0 && jobs.every((f) => f.installed))
  const dlPaused = $derived(jobs.some((f) => f.job?.state === 'paused'))
  const dlFailed = $derived(jobs.find((f) => f.job?.state === 'failed'))

  // Download speed, smoothed, for the time left.
  let rate = $state(0)
  let lastGot = 0
  let lastAt = 0
  $effect(() => {
    const got = dlGot
    const now = performance.now()
    if (lastAt && got > lastGot) {
      const r = ((got - lastGot) / (now - lastAt)) * 1000
      rate = rate ? rate * 0.8 + r * 0.2 : r
    }
    lastGot = got
    lastAt = now
  })
  const timeLeft = $derived.by(() => {
    if (!rate || dlDone) return ''
    const s = (dlTotal - dlGot) / rate
    return s < 60 ? 'less than a minute left' : `about ${Math.ceil(s / 60)} min left`
  })

  function startDownloads() {
    for (const f of toGet) downloadFeature(f.feature)
    mine = [...new Set([...mine, ...toGet.map((f) => f.feature)])]
    skipped = false
    step = 3
  }

  function notNow() {
    skipped = true
    step = 3
  }

  function toggleAll() {
    for (const f of jobs) if (!f.installed) dlPaused ? downloadFeature(f.feature) : pauseFeature(f.feature)
  }

  async function addFolder() {
    folderError = ''
    const path = await open({ directory: true, multiple: false, title: 'Choose a folder with photos or videos' })
    if (typeof path !== 'string') return
    if (folders.some((f) => f.path.toLowerCase() === path.toLowerCase())) return
    adding = true
    try {
      const keepInfo = await askKeepInfo(path)
      const folder = await api.folders.add(path, keepInfo)
      folders = [...folders, folder]
    } catch (e) {
      folderError = errorMessage(e)
    } finally {
      adding = false
    }
  }

  const s = $derived(library.status)
  const scanning = $derived(!!s && (s.scanning || s.pending > 0))
  const found = $derived(s ? (s.scanning ? s.scannedFiles : s.photos + s.videos) : 0)

  async function finish() {
    step = 4
    try {
      await api.settings.update({ welcomeDone: true })
    } catch {
      // Shown again next time; nothing else depends on it.
    }
    setTimeout(close, 1800)
  }

  function close() {
    if (leaving) return
    leaving = true
    setTimeout(ondone, 260)
  }

  const doneText = $derived.by(() => {
    let t = folders.length ? 'Memora is sorting your photos. They appear as they are found.' : 'Add a folder in Settings whenever you are ready.'
    if (started && !dlDone) t += ' AI features finish downloading in the background.'
    if (skipped) t += ' AI features can be downloaded any time in Settings.'
    return t
  })

  const TILES: [string, string][] = [
    ['#fde7c8', 'M3 18h18M6 18a6 6 0 0 1 12 0M12 7V4M5.6 9.6 3.5 7.5M18.4 9.6l2.1-2.1'],
    ['#d7e8fb', 'M3 19l6-8 4 5 3-3 5 6zM16 7a2 2 0 1 0 0-4 2 2 0 0 0 0 4z'],
    ['#f6dbe6', 'M12 21s-7-4.5-7-10a4 4 0 0 1 7-2.6A4 4 0 0 1 19 11c0 5.5-7 10-7 10z'],
    ['#dcefdc', 'M12 14a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM12 8V3M12 21v-7M8 11c-3 0-5-2-5-2s2-2 5-2M16 11c3 0 5-2 5-2s-2-2-5-2'],
    ['#e8e1f5', 'M16 19v-1a4 4 0 0 0-8 0v1M12 11a3 3 0 1 0 0-6 3 3 0 0 0 0 6z'],
    ['#fff1bf', 'M4 13h16a8 8 0 0 1-16 0zM8 9c0-2 2-2 2-4M13 9c0-2 2-2 2-4'],
    ['#d4ecee', 'M12 3v18M5 7l14 10M19 7 5 17'],
    ['#f3e0d3', 'M4 21V9l8-5 8 5v12M9 21v-6h6v6'],
    ['#e2e6ea', 'M20 14A8 8 0 1 1 10 4a6 6 0 0 0 10 10z'],
  ]
  const INK = ['#b06000', '#1a73e8', '#a50e5a', '#137333', '#5b3fa8', '#8a5a00', '#0b6e77', '#8c3d10', '#3c4043']
</script>

<div class="welcome" class:leaving role="dialog" aria-modal="true" aria-label="Welcome to Memora">
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <header class="w-bar" data-tauri-drag-region="deep" oncontextmenu={titleBarContextMenu}>
    <img src="/favicon.svg" alt="" width="20" height="20" />
    <span>Memora</span>
  </header>

  {#if step <= 3}
    <nav class="w-steps" aria-label="Setup steps">
      {#each [[1, 'Welcome'], [2, 'AI features'], [3, 'Your photos']] as const as [n, label], i (n)}
        {#if i > 0}<span class="w-line" class:full={step > n - 1}><i></i></span>{/if}
        <span class="w-pip" class:on={step === n} class:done={step > n} aria-current={step === n ? 'step' : undefined}>
          <b>{#if step > n}<Icon name="check" size={14} />{:else}{n}{/if}</b>{label}
        </span>
      {/each}
    </nav>
  {/if}

  <div class="w-stage">
    {#if step === 1}
      <section class="w-step" aria-labelledby="w-title">
        <div class="w-hello">
          <div>
            <div class="w-logo"><img src="/favicon.svg" alt="" width="36" height="36" /></div>
            <h1 id="w-title" class="rise d1">Welcome to Memora</h1>
            <p class="muted rise d2 lead">All your photos and videos in one place, sorted by date, people and places.</p>
            <ul class="w-points">
              <li class="rise d3"><Icon name="lock" size={20} /><span><strong>Private.</strong> <span class="muted">Your photos stay on this PC. Nothing is uploaded.</span></span></li>
              <li class="rise d4"><Icon name="explore" size={20} /><span><strong>Works offline.</strong> <span class="muted">No account and no internet needed after setup.</span></span></li>
              <li class="rise d5"><Icon name="folder" size={20} /><span><strong>Your files stay put.</strong> <span class="muted">Memora reads your folders. It never moves or changes your files.</span></span></li>
            </ul>
            <div class="rise d5">
              <button class="btn primary w-big" onclick={() => (step = 2)}>Get started <Icon name="chevronRight" size={20} /></button>
            </div>
          </div>
          <div class="w-mosaic" aria-hidden="true">
            {#each TILES as [bg, d], i (i)}
              <div class="w-tile" style:background={bg} style:animation-delay={`${0.1 + i * 0.07}s, ${i * 0.4}s`}>
                <svg viewBox="0 0 24 24" fill="none" stroke={INK[i]} stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path {d} /></svg>
              </div>
            {/each}
          </div>
        </div>
      </section>
    {:else if step === 2}
      <section class="w-step" aria-labelledby="m-title">
        <h2 id="m-title" class="rise">Choose your AI features</h2>
        <p class="muted rise d1 w-sub">
          {#if bundled}
            These came with this copy of Memora and run on this PC.
          {:else}
            These run on this PC, so they need a one-time download. Untick anything you don't want. You can change this later in Settings.
          {/if}
        </p>
        <div class="w-models">
          {#each FEATURES as f, i (f)}
            {@const st = list.find((x) => x.feature === f)}
            {@const have = !!st?.installed}
            {@const on = have || chosen[f]}
            <label class="w-model rise d{i + 1}" class:on class:fixed={have}>
              <input type="checkbox" checked={on} disabled={have} onchange={() => (chosen[f] = !chosen[f])} aria-describedby="desc-{f}" />
              <span class="w-ico tint-{FEATURE_INFO[f].tint}"><Icon name={FEATURE_INFO[f].icon} size={24} /></span>
              <span>
                <span class="w-mtitle">
                  {FEATURE_INFO[f].title}
                  {#if have}<span class="w-badge">Installed</span>{:else if st?.job}<span class="w-badge dl">Downloading</span>{/if}
                </span>
                <span class="w-mtext" id="desc-{f}">{FEATURE_INFO[f].text}</span>
                {#if f === 'blur' && blurNote}<span class="w-mnote"><Icon name="info" size={14} />{blurNote}</span>{/if}
              </span>
              <span class="w-mside">
                <span class="muted small">{st && !have ? mb(st.size) : ''}</span>
                <span class="w-box"><Icon name="check" size={16} /></span>
              </span>
            </label>
          {/each}
        </div>
        <div class="w-foot rise d4">
          <span class="muted small w-total">
            <Icon name="download" size={16} />
            {#if bundled}
              Already included. Nothing to download.
            {:else if total > 0}
              {mb(total)} to download
            {:else}
              Nothing selected
            {/if}
          </span>
          <span class="w-actions">
            <button class="btn w-text" onclick={() => (step = 1)}>Back</button>
            {#if !bundled}<button class="btn w-text" onclick={notNow}>Not now</button>{/if}
            <button class="btn primary" disabled={!bundled && total === 0 && !started} onclick={startDownloads}>
              {bundled || total === 0 ? 'Continue' : 'Download and continue'}
            </button>
          </span>
        </div>
        {#if !bundled}
          <p class="muted small w-privacy rise d5"><Icon name="lock" size={14} /> The files come from Hugging Face. Your photos are never sent anywhere.</p>
        {/if}
      </section>
    {:else if step === 3}
      <section class="w-step top" aria-labelledby="f-title">
        <h2 id="f-title" class="rise">Where are your photos?</h2>
        <p class="muted rise d1 w-sub">Add the folders you keep photos and videos in. Subfolders are included.</p>

        {#if folders.length === 0}
          <div class="w-drop rise d2">
            <span class="w-folder"><Icon name="folder" size={56} /></span>
            <button class="btn primary w-big" disabled={adding} onclick={() => addFolder()}>
              {#if adding}<span class="spinner" aria-hidden="true"></span>{/if} Choose a folder…
            </button>
          </div>
        {:else}
          <div class="w-rows">
            {#each folders as f (f.id)}
              <div class="w-row">
                <span class="w-row-ico"><Icon name="folder" size={24} /></span>
                <span class="w-grow">
                  <strong>{f.path.split(/[\\/]/).filter(Boolean).pop() ?? f.path}</strong>
                  <span class="w-path">{f.path}</span>
                </span>
              </div>
            {/each}
            <div class="w-count muted small" role="status">
              {#if scanning}<span class="spinner" aria-hidden="true"></span> Finding photos… {found.toLocaleString()} so far
              {:else}{found.toLocaleString()} photos and videos{/if}
            </div>
            <div><button class="btn w-text" disabled={adding} onclick={() => addFolder()}><Icon name="add" size={18} /> Add another folder</button></div>
          </div>
        {/if}
        {#if folderError}<p class="error">{folderError}</p>{/if}

        {#if jobs.length > 0}
          <div class="w-dl rise d3" role="status">
            <div class="w-dlhead">
              {#if dlDone}<Icon name="checkCircle" size={20} />{:else if !dlPaused && !dlFailed}<span class="spinner" aria-hidden="true"></span>{/if}
              <span class="w-grow">
                <strong>
                  {dlDone ? 'AI features are ready' : dlFailed ? 'Download stopped' : dlPaused ? 'Download paused' : 'Downloading AI features'}
                </strong>
                {#if !dlDone}
                  <span class="muted">
                    {mb(dlGot)} of {mb(dlTotal)}{#if dlFailed}: {dlFailed.job?.error}{:else if !dlPaused && timeLeft}{' · '}{timeLeft} · you can carry on meanwhile{/if}
                  </span>
                {/if}
              </span>
              {#if !dlDone}
                <button class="btn w-text" onclick={toggleAll}>{dlPaused || dlFailed ? (dlFailed ? 'Try again' : 'Resume') : 'Pause'}</button>
              {/if}
            </div>
            <div class="w-dlitems">
              {#each jobs as f (f.feature)}
                {@const pct = f.installed ? 100 : f.job ? Math.floor((f.job.got / Math.max(1, f.job.total)) * 100) : 0}
                <div class="w-dlitem">
                  <span class="w-dlname"><span>{FEATURE_INFO[f.feature].title}</span><span class="muted">{f.installed ? 'Ready' : f.job?.state === 'queued' ? 'Waiting' : `${pct}%`}</span></span>
                  <span class="w-track" class:ok={f.installed}><i style:width="{pct}%"></i></span>
                </div>
              {/each}
            </div>
          </div>
        {:else if skipped}
          <p class="muted small w-hint rise d3"><Icon name="info" size={16} /> AI features are off. Download them any time in Settings › AI features.</p>
        {/if}

        <div class="w-foot end">
          <button class="btn w-text" onclick={() => (step = 2)}>Back</button>
          {#if folders.length === 0}<button class="btn w-text" onclick={finish}>Skip for now</button>{/if}
          <button class="btn primary" disabled={folders.length === 0} onclick={finish}>Open my library</button>
        </div>
      </section>
    {:else}
      <section class="w-step" aria-labelledby="d-title">
        <div class="w-done">
          <div class="w-burst"><Icon name="check" size={44} /></div>
          <h2 id="d-title" class="rise d1">You're all set</h2>
          <p class="muted rise d2">{doneText}</p>
          <button class="btn primary rise d3" onclick={close}>Start browsing</button>
        </div>
      </section>
    {/if}
  </div>
</div>

<style>
  .welcome {
    position: fixed;
    inset: 0;
    z-index: 180;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    color: var(--fg);
    animation: w-in 0.3s var(--m-ease) both;
  }

  .welcome.leaving {
    animation: w-out 0.26s var(--m-ease) both;
  }

  @keyframes w-in {
    from {
      opacity: 0;
    }
  }

  @keyframes w-out {
    to {
      opacity: 0;
      transform: scale(1.02);
    }
  }

  .w-bar {
    height: var(--topbar-h);
    flex: none;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 calc(8px + var(--wc-w)) 0 20px;
    font-size: 13px;
    color: var(--fg-muted);
    user-select: none;
  }

  .w-steps {
    display: flex;
    justify-content: center;
    gap: 8px;
    flex: none;
  }

  .w-pip {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 14px 6px 6px;
    border-radius: 999px;
    font-size: 13px;
    color: var(--fg-muted);
    transition:
      background 0.3s,
      color 0.3s;
  }

  .w-pip b {
    width: 24px;
    height: 24px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    font-size: 12px;
    font-weight: 600;
    background: var(--bg-hover);
    transition:
      background 0.3s,
      color 0.3s;
  }

  .w-pip.on {
    background: var(--accent-soft);
    color: var(--fg);
  }

  .w-pip.on b {
    background: var(--accent);
    color: var(--accent-fg);
  }

  .w-pip.done b {
    background: var(--ok-soft);
    color: var(--ok);
  }

  .w-line {
    width: 40px;
    height: 2px;
    align-self: center;
    border-radius: 2px;
    background: var(--border);
    position: relative;
    overflow: hidden;
  }

  .w-line i {
    position: absolute;
    inset: 0;
    background: var(--accent);
    transform: scaleX(0);
    transform-origin: left;
    transition: transform 0.5s var(--m-ease);
  }

  .w-line.full i {
    transform: scaleX(1);
  }

  .w-stage {
    flex: 1;
    min-height: 0;
    position: relative;
    overflow: auto;
  }

  .w-step {
    min-height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: 24px 48px 40px;
    animation: w-step 0.55s var(--m-ease-out) both;
  }

  .w-step.top {
    justify-content: flex-start;
    padding-top: 48px;
  }

  @keyframes w-step {
    from {
      opacity: 0;
      transform: translateX(48px);
    }
  }

  .rise {
    animation: w-rise 0.6s var(--m-ease-out) both;
  }

  @keyframes w-rise {
    from {
      opacity: 0;
      transform: translateY(16px);
    }
  }

  .d1 {
    animation-delay: 0.08s;
  }
  .d2 {
    animation-delay: 0.16s;
  }
  .d3 {
    animation-delay: 0.24s;
  }
  .d4 {
    animation-delay: 0.32s;
  }
  .d5 {
    animation-delay: 0.4s;
  }

  h1 {
    font-size: 36px;
    line-height: 1.2;
    font-weight: 500;
    margin: 0;
  }

  h2 {
    font-size: 26px;
    line-height: 1.25;
    font-weight: 500;
    margin: 0;
  }

  .lead {
    font-size: 16px;
    margin: 12px 0 0;
  }

  .w-sub {
    margin: 8px 0 0;
    max-width: 580px;
    text-align: center;
  }

  .w-big {
    height: 40px;
    padding: 0 24px;
  }

  .w-text {
    border-color: transparent;
  }

  .w-hello {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: 56px;
    align-items: center;
    width: 100%;
    max-width: 1000px;
  }

  .w-logo {
    width: 60px;
    height: 60px;
    border-radius: 18px;
    background: var(--accent-soft);
    display: grid;
    place-items: center;
    margin-bottom: 24px;
    animation: w-pop 0.7s var(--m-spring) both;
  }

  @keyframes w-pop {
    from {
      opacity: 0;
      transform: scale(0.4) rotate(-20deg);
    }
  }

  .w-points {
    list-style: none;
    padding: 0;
    margin: 24px 0 32px;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }

  .w-points li {
    display: flex;
    gap: 12px;
    align-items: flex-start;
  }

  .w-points :global(svg) {
    flex: none;
    color: var(--accent);
    margin-top: 1px;
  }

  .w-mosaic {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 12px;
    transform: rotate(-4deg);
  }

  .w-tile {
    aspect-ratio: 1;
    border-radius: 18px;
    display: grid;
    place-items: center;
    box-shadow: 0 1px 2px rgb(60 64 67 / 0.12);
    animation:
      w-tile 0.8s cubic-bezier(0.34, 1.45, 0.64, 1) both,
      w-bob 6s ease-in-out infinite;
  }

  .w-tile:nth-child(3n + 2) {
    margin-top: 28px;
  }

  .w-tile svg {
    width: 44%;
    height: 44%;
  }

  @keyframes w-tile {
    from {
      opacity: 0;
      transform: translateY(40px) scale(0.7);
    }
  }

  @keyframes w-bob {
    50% {
      translate: 0 -8px;
    }
  }

  .w-models {
    display: flex;
    flex-direction: column;
    gap: 12px;
    width: 100%;
    max-width: 720px;
    margin: 24px 0 16px;
  }

  .w-model {
    display: grid;
    grid-template-columns: 48px minmax(0, 1fr) auto;
    gap: 16px;
    align-items: center;
    padding: 16px 20px;
    border: 1px solid var(--border);
    border-radius: 16px;
    cursor: pointer;
    position: relative;
    transition:
      border-color 0.2s,
      background 0.2s,
      box-shadow 0.2s,
      transform 0.15s;
  }

  .w-model:hover {
    box-shadow: 0 1px 3px rgb(60 64 67 / 0.2);
  }

  .w-model:active:not(.fixed) {
    transform: scale(0.99);
  }

  .w-model.on {
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent-soft) 40%, transparent);
  }

  .w-model.fixed {
    cursor: default;
  }

  .w-model:has(input:focus-visible) {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .w-model input {
    position: absolute;
    opacity: 0;
    width: 1px;
    height: 1px;
  }

  .w-ico {
    width: 48px;
    height: 48px;
    border-radius: 14px;
    display: grid;
    place-items: center;
  }

  .tint-blue {
    background: var(--accent-soft);
    color: var(--accent);
  }

  .tint-amber {
    background: var(--warn-bg);
    color: var(--warn);
  }

  .tint-green {
    background: var(--ok-soft);
    color: var(--ok);
  }

  .w-mtitle {
    display: flex;
    gap: 8px;
    align-items: center;
    flex-wrap: wrap;
    font-weight: 600;
    font-size: 15px;
  }

  .w-mtext {
    display: block;
    color: var(--fg-muted);
    margin-top: 2px;
  }

  .w-mnote {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 6px;
    font-size: 12px;
    color: var(--fg-muted);
  }

  .w-mside {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 8px;
    white-space: nowrap;
  }

  .w-box {
    width: 22px;
    height: 22px;
    border-radius: 6px;
    border: 2px solid var(--fg-muted);
    display: grid;
    place-items: center;
    color: var(--accent-fg);
    transition:
      background 0.2s,
      border-color 0.2s;
  }

  .w-box :global(svg) {
    transform: scale(0);
    transition: transform 0.3s var(--m-spring);
  }

  .w-model.on .w-box {
    background: var(--accent);
    border-color: var(--accent);
  }

  .w-model.on .w-box :global(svg) {
    transform: scale(1);
  }

  .w-badge {
    font-size: 11px;
    font-weight: 600;
    padding: 1px 8px;
    border-radius: 999px;
    background: var(--ok-soft);
    color: var(--ok);
  }

  .w-badge.dl {
    background: var(--accent-soft);
    color: var(--accent);
  }

  .w-foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    width: 100%;
    max-width: 720px;
  }

  .w-foot.end {
    justify-content: flex-end;
    gap: 8px;
  }

  .w-total,
  .w-privacy,
  .w-hint {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .w-privacy {
    margin-top: 20px;
  }

  .w-hint {
    margin: 0 0 20px;
  }

  .w-actions {
    display: flex;
    gap: 8px;
  }

  .w-drop {
    width: 100%;
    max-width: 720px;
    border: 2px dashed var(--border);
    border-radius: 20px;
    padding: 28px;
    margin: 24px 0 16px;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    transition:
      border-color 0.2s,
      background 0.2s;
  }

  .w-drop:hover {
    border-color: var(--accent);
  }

  .w-folder {
    color: var(--accent);
    animation: w-float 3s ease-in-out infinite;
  }

  @keyframes w-float {
    50% {
      transform: translateY(-6px);
    }
  }

  .w-rows {
    width: 100%;
    max-width: 720px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin: 24px 0 16px;
  }

  .w-row {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 12px 16px;
    border-radius: 14px;
    background: var(--bg-elev);
    animation: w-rise 0.45s var(--m-ease-out) both;
  }

  .w-row-ico {
    color: var(--accent);
    display: grid;
  }

  .w-grow {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }

  .w-path {
    font-size: 12px;
    color: var(--fg-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .w-count {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 4px;
  }

  .w-dl {
    width: 100%;
    max-width: 720px;
    border: 1px solid var(--border);
    border-radius: 16px;
    padding: 14px 18px;
    margin-bottom: 20px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .w-dlhead {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 13px;
  }

  .w-dlhead > :global(svg) {
    color: var(--ok);
  }

  .w-dlhead .w-grow {
    display: block;
  }

  .w-dlitems {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 12px;
  }

  .w-dlitem {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
  }

  .w-dlname {
    display: flex;
    justify-content: space-between;
    gap: 8px;
  }

  .w-track {
    height: 6px;
    border-radius: 999px;
    background: var(--bg-hover);
    overflow: hidden;
  }

  .w-track i {
    display: block;
    height: 100%;
    border-radius: 999px;
    background: var(--accent);
    transition: width 0.4s linear;
  }

  .w-track.ok i {
    background: var(--ok);
  }

  .w-done {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 16px;
    text-align: center;
    max-width: 480px;
  }

  .w-done p {
    margin: 0;
  }

  .w-burst {
    width: 88px;
    height: 88px;
    border-radius: 50%;
    background: var(--ok-soft);
    color: var(--ok);
    display: grid;
    place-items: center;
    position: relative;
    animation: w-pop 0.7s var(--m-spring) both;
  }

  .w-burst::after {
    content: '';
    position: absolute;
    inset: -6px;
    border-radius: 50%;
    border: 2px solid var(--ok);
    animation: w-ring 1.2s ease-out 0.2s both;
  }

  @keyframes w-ring {
    from {
      opacity: 0.8;
      transform: scale(0.8);
    }
    to {
      opacity: 0;
      transform: scale(1.6);
    }
  }

  @media (max-width: 860px) {
    .w-hello {
      grid-template-columns: minmax(0, 1fr);
    }
    .w-mosaic {
      display: none;
    }
    .w-dlitems {
      grid-template-columns: minmax(0, 1fr);
    }
  }
</style>
