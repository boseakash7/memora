<script lang="ts">
  import { listen } from '@tauri-apps/api/event'
  import { open } from '@tauri-apps/plugin-dialog'
  import {
    dataFolder,
    errorMessage,
    showInExplorer,
    type DataFolder,
    type DataMoveError,
    type DataMoveProgress,
  } from '../lib/api'
  import { formatBytes } from '../lib/dates'
  import { confirmDialog } from '../lib/dialogs.svelte'
  import Icon from '../lib/Icon.svelte'

  let folder = $state<DataFolder | null>(null)
  let error = $state('')
  /** While moving: progress, or null before the first report. */
  let moving = $state<{ progress: DataMoveProgress | null } | null>(null)
  /** The move failed after background work stopped: only a restart helps. */
  let mustRestart = $state('')

  $effect(() => {
    dataFolder
      .get()
      .then((f) => (folder = f))
      .catch((e) => (error = errorMessage(e)))
  })

  async function move() {
    error = ''
    const picked = await open({ directory: true, multiple: false, title: 'Choose where to keep the library data' })
    if (typeof picked !== 'string') return
    let target
    try {
      target = await dataFolder.target(picked)
    } catch (e) {
      error = `Can't move there: ${errorMessage(e)}.`
      return
    }
    const ok = await confirmDialog({
      title: 'Move the library data?',
      message:
        `${formatBytes(target.size)} (database and thumbnails) is copied to ${target.target}. ` +
        'Memora restarts when it is done, and the old copy is deleted. Your photos stay where they are.',
      confirmLabel: 'Move',
    })
    if (!ok) return

    moving = { progress: null }
    const unlisten = await listen<DataMoveProgress>('data-move', (e) => (moving = { progress: e.payload }))
    try {
      await dataFolder.move(picked) // restarts the app on success
    } catch (e) {
      const err = e as DataMoveError
      if (err && typeof err === 'object' && 'restart' in err) {
        if (err.restart) mustRestart = err.message
        else error = `The library data was not moved: ${err.message}.`
      } else {
        error = `The library data was not moved: ${errorMessage(e)}.`
      }
      moving = null
    } finally {
      unlisten()
    }
  }

  const percent = $derived(
    moving?.progress && moving.progress.total > 0
      ? Math.min(100, Math.round((moving.progress.done / moving.progress.total) * 100))
      : 0,
  )
</script>

<div class="card">
  <div class="card-head">
    <div>
      <h2>Storage</h2>
      <p class="muted">
        The library data (database and thumbnails) can live on another drive, for example when this one is getting full.
      </p>
    </div>
    <button class="btn" onclick={move} disabled={!folder || moving !== null}>
      <Icon name="folder" size={20} /> Move…
    </button>
  </div>
  {#if error}<p class="error">{error}</p>{/if}
  {#if folder}
    <p class="muted">
      <button class="link" onclick={() => showInExplorer(folder!.dataDir)}>{folder.dataDir}</button>
      · {formatBytes(folder.size)}
    </p>
    <p class="muted small">
      Network and cloud-synced folders (OneDrive, Dropbox, Google Drive) can't be used: the database can be damaged there.
    </p>
  {/if}
</div>

{#if moving || mustRestart}
  <div class="modal-backdrop">
    <div class="modal" role="dialog" aria-modal="true" aria-label="Moving the library data">
      {#if mustRestart}
        <h2>The library data was not moved</h2>
        <p>{mustRestart}.</p>
        <p class="muted">Nothing was changed. Memora needs to restart to carry on.</p>
        <div class="modal-actions">
          <button class="btn primary" onclick={() => dataFolder.restart()}>Restart Memora</button>
        </div>
      {:else}
        <h2>Moving the library data…</h2>
        <progress class="move-progress" max="100" value={percent}></progress>
        <p class="muted">
          {#if moving?.progress?.phase === 'finish'}
            Finishing up. Memora restarts in a moment.
          {:else if moving?.progress}
            {formatBytes(moving.progress.done)} of {formatBytes(moving.progress.total)} copied
          {:else}
            Getting started…
          {/if}
        </p>
      {/if}
    </div>
  </div>
{/if}

<style>
  .move-progress {
    width: 100%;
    height: 8px;
    margin: 8px 0;
    accent-color: var(--accent);
  }
  .small {
    font-size: 13px;
  }
</style>
