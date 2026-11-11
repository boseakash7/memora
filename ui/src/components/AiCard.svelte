<script lang="ts">
  import { untrack } from 'svelte'
  import { api, errorMessage, showInExplorer, type AiStatus, type Settings } from '../lib/api'
  import Icon from '../lib/Icon.svelte'
  import { library } from '../lib/library.svelte'

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

  async function checkAgain() {
    try {
      ai = await api.ai.refresh()
      setTimeout(load, 500)
    } catch (e) {
      error = errorMessage(e)
    }
  }

  const where = $derived(ai?.device === 'gpu' ? 'on the graphics card' : 'on the processor')
  const pending = $derived(library.status?.aiPending ?? ai?.pending ?? 0)
</script>

<div class="card">
  <div class="card-head">
    <div>
      <h2>People and smart search</h2>
      <p class="muted">
        Finds faces and groups them into people, and lets you search by what is in a photo (“beach”, “dog”, “birthday
        cake”). It runs on this PC; nothing is uploaded.
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

  {#if settings.mlEnabled && ai}
    <div class="ai-status">
      {#if ai.state === 'noModels'}
        <p><Icon name="warning" size={18} /> The recognition model files are not installed.</p>
        <p class="muted small">Copy them into one of these folders, then choose Check again:</p>
        <ul class="small">
          {#each ai.searched as dir (dir)}
            <li><button class="link" onclick={() => showInExplorer(dir).catch(() => {})}>{dir}</button></li>
          {/each}
        </ul>
        <button class="btn" onclick={checkAgain}><Icon name="refresh" size={20} /> Check again</button>
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

  .ai-status ul {
    margin: 4px 0 12px;
    padding-left: 20px;
  }
</style>
