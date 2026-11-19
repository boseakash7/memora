<script lang="ts">
  import { untrack } from 'svelte'
  import { ask } from '@tauri-apps/plugin-dialog'
  import { api, errorMessage, type AiStatus, type Feature, type Settings } from '../lib/api'
  import Icon from '../lib/Icon.svelte'
  import { library } from '../lib/library.svelte'
  import {
    FEATURE_INFO,
    FEATURES,
    cancelFeature,
    downloadFeature,
    features,
    mb,
    pauseFeature,
    removeFeature,
    watchFeatures,
  } from '../lib/features.svelte'

  let { settings, onchange }: { settings: Settings; onchange: (s: Settings) => void } = $props()

  let ai = $state<AiStatus | null>(null)
  let error = $state('')
  let busy = $state(false)

  function load() {
    api.ai
      .status()
      .then((s) => (ai = s))
      .catch((e) => (error = errorMessage(e)))
  }

  // Follow progress; the worker also changes state without the library count moving
  // (loading models, idle), so look again now and then while the page is open.
  $effect(() => {
    library.status?.aiPending
    library.status?.pending
    untrack(load)
  })
  $effect(() => {
    const timer = setInterval(load, 3000)
    return () => clearInterval(timer)
  })
  $effect(() => watchFeatures())

  async function save(patch: Partial<Settings>) {
    busy = true
    error = ''
    try {
      onchange(await api.settings.update(patch))
      setTimeout(load, 300)
    } catch (e) {
      error = errorMessage(e)
    } finally {
      busy = false
    }
  }

  /** Model files copied in by hand. */
  async function checkAgain() {
    try {
      ai = await api.ai.refresh()
      setTimeout(load, 500)
    } catch (e) {
      error = errorMessage(e)
    }
  }

  async function remove(f: Feature) {
    const ok = await ask(
      `Remove ${FEATURE_INFO[f].title}? It turns off until you download it again. What it already found, like people's names, is kept.`,
      { title: 'Remove AI feature', kind: 'warning', okLabel: 'Remove', cancelLabel: 'Cancel' },
    )
    if (ok) removeFeature(f)
  }

  const list = $derived(features.list ?? [])
  const missing = $derived(list.filter((f) => !f.installed && !f.job))
  const used = $derived(list.filter((f) => f.installed && f.removable).reduce((n, f) => n + f.size, 0))
  const where = $derived(ai?.device === 'gpu' ? 'on the graphics card' : 'on the processor')
  const pending = $derived(library.status?.aiPending ?? ai?.pending ?? 0)
  const pct = (got: number, total: number) => Math.floor((got / Math.max(1, total)) * 100)
</script>

<div class="card">
  <div class="card-head">
    <div>
      <h2>AI features</h2>
      <p class="muted">
        Groups photos by the people in them, finds photos by what is in them and helps in the editor. Each feature needs its
        own one-time download. Photos are looked at on this PC only; nothing is uploaded.
      </p>
    </div>
    <label class="switch" title={settings.mlEnabled ? 'Turn off' : 'Turn on'}>
      <input
        type="checkbox"
        checked={settings.mlEnabled}
        disabled={busy}
        onchange={(e) => save({ mlEnabled: e.currentTarget.checked })}
      />
      <span class="slider"></span>
    </label>
  </div>

  {#if error}<p class="error">{error}</p>{/if}

  {#if settings.mlEnabled && list.length > 0}
    <ul class="features">
      {#each FEATURES as f (f)}
        {@const st = list.find((x) => x.feature === f)}
        {#if st}
          {@const job = st.job}
          {@const p = job ? pct(job.got, job.total) : 0}
          <li class="feature">
            <span class="f-ico tint-{FEATURE_INFO[f].tint}"><Icon name={FEATURE_INFO[f].icon} size={22} /></span>
            <span class="f-body">
              <span class="f-title">
                {FEATURE_INFO[f].title}
                {#if st.installed}
                  <span class="f-badge ok">On</span>
                {:else if job?.state === 'downloading'}
                  <span class="f-badge dl">Downloading {p}%</span>
                {:else if job?.state === 'queued'}
                  <span class="f-badge dl">Waiting</span>
                {:else if job?.state === 'paused'}
                  <span class="f-badge">Paused</span>
                {:else if job?.state === 'failed'}
                  <span class="f-badge err">Stopped</span>
                {:else}
                  <span class="f-badge">Not downloaded</span>
                {/if}
              </span>
              {#if job?.state === 'failed'}
                <span class="error small">{job.error}</span>
              {:else}
                <span class="muted small">
                  {FEATURE_INFO[f].short}
                  {#if f === 'blur' && !st.installed && !st.full}Lighter version for this PC.{/if}
                </span>
              {/if}
              {#if job && job.state !== 'failed'}
                <span class="f-track" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={p} aria-label={FEATURE_INFO[f].title}>
                  <i style:width="{p}%"></i>
                </span>
              {/if}
            </span>
            <span class="f-side">
              <span class="muted small">{job ? `${mb(job.got)} / ${mb(job.total)}` : mb(st.size)}</span>
              {#if st.installed}
                {#if st.removable}<button class="btn danger" onclick={() => remove(f)}>Remove</button>{/if}
              {:else if !job}
                <button class="btn primary" onclick={() => downloadFeature(f)}><Icon name="download" size={18} /> Download</button>
              {:else}
                {#if job.state === 'paused' || job.state === 'failed'}
                  <button class="btn" onclick={() => downloadFeature(f)}>{job.state === 'failed' ? 'Try again' : 'Resume'}</button>
                {:else}
                  <button class="btn" onclick={() => pauseFeature(f)}>Pause</button>
                {/if}
                <button class="btn" onclick={() => cancelFeature(f)}>Cancel</button>
              {/if}
            </span>
          </li>
        {/if}
      {/each}
    </ul>
    <div class="f-foot">
      <span class="muted small">{used > 0 ? `Downloaded models use ${mb(used)}.` : ''}</span>
      <span class="f-foot-actions">
        <button class="link small" onclick={checkAgain} title="Copied model files into the models folder by hand?">Check again</button>
        {#if missing.length > 1}
          <button class="btn primary" onclick={() => missing.forEach((f) => downloadFeature(f.feature))}>
            Download all ({mb(missing.reduce((n, f) => n + f.size, 0))})
          </button>
        {/if}
      </span>
    </div>
  {/if}

  {#if settings.mlEnabled && ai}
    <div class="ai-status">
      {#if ai.state === 'noModels'}
        <p class="muted">Download People and faces or Search above to start looking at your photos.</p>
      {:else if ai.state === 'error'}
        <p class="error">Could not start: {ai.error}</p>
      {:else if ai.state === 'waiting'}
        <p><span class="spinner"></span> Starts when indexing is done ({pending.toLocaleString()} to look at).</p>
      {:else if ai.state === 'working' || (ai.state === 'starting' && pending > 0)}
        <p><span class="spinner"></span> Looking at photos {where}… {pending.toLocaleString()} left</p>
      {:else if ai.state === 'idle'}
        <p><Icon name="check" size={18} /> Up to date. Runs {where}.</p>
      {:else}
        <p class="muted">Starting…</p>
      {/if}
    </div>

    <div class="options">
      <label class="check">
        <input
          type="checkbox"
          checked={settings.mlUseGpu}
          disabled={busy}
          onchange={(e) => save({ mlUseGpu: e.currentTarget.checked })}
        />
        <span>
          Use the graphics card
          <span class="muted small">Faster. Turn off if the PC gets noisy or other apps stutter while it works.</span>
        </span>
      </label>
    </div>
  {/if}
</div>

<style>
  .features {
    list-style: none;
    margin: 12px 0 0;
    padding: 0;
  }

  .feature {
    display: grid;
    grid-template-columns: 40px minmax(0, 1fr) auto;
    gap: 16px;
    align-items: center;
    padding: 14px 0;
    border-top: 1px solid var(--border);
  }

  .feature:first-child {
    border-top: 0;
  }

  .f-ico {
    width: 40px;
    height: 40px;
    border-radius: 12px;
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

  .f-body {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .f-title {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    font-weight: 600;
  }

  .f-badge {
    font-size: 11px;
    font-weight: 600;
    padding: 1px 8px;
    border-radius: 999px;
    background: var(--bg-hover);
    color: var(--fg-muted);
  }

  .f-badge.ok {
    background: var(--ok-soft);
    color: var(--ok);
  }

  .f-badge.dl {
    background: var(--accent-soft);
    color: var(--accent);
  }

  .f-badge.err {
    color: var(--danger);
  }

  .f-track {
    display: block;
    height: 4px;
    max-width: 360px;
    margin-top: 8px;
    border-radius: 999px;
    background: var(--bg-hover);
    overflow: hidden;
  }

  .f-track i {
    display: block;
    height: 100%;
    background: var(--accent);
    transition: width 0.4s linear;
  }

  .f-side {
    display: flex;
    align-items: center;
    gap: 8px;
    white-space: nowrap;
  }

  .f-foot {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 16px;
    padding: 12px 0 4px;
    border-top: 1px solid var(--border);
  }

  .f-foot-actions {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .ai-status {
    margin: 8px 0 12px;
    padding: 12px 16px;
    border-radius: 10px;
    background: var(--bg-elev);
  }

  .ai-status p {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 4px 0;
  }
</style>
