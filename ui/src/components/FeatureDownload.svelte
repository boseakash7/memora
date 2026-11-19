<script lang="ts">
  import type { Feature } from '../lib/api'
  import Icon from '../lib/Icon.svelte'
  import { FEATURE_INFO, cancelFeature, downloadFeature, feature, features, mb, pauseFeature, watchFeatures } from '../lib/features.svelte'

  /**
   * Download a feature's models from where the feature would be: a button with the size,
   * then progress with Pause/Cancel. Nothing once installed (the feature takes over).
   */
  let {
    of,
    label = 'Download',
    compact = false,
    inline = false,
  }: { of: Feature; label?: string; compact?: boolean; inline?: boolean } = $props()

  $effect(() => watchFeatures())

  const st = $derived(features.list ? feature(of) : undefined)
  const job = $derived(st?.job ?? null)
  const pct = $derived(job ? Math.floor((job.got / Math.max(1, job.total)) * 100) : 0)
</script>

{#if st && !st.installed}
  <div class="fd" class:compact class:inline>
    {#if !job}
      <button class="btn" class:primary={!inline} onclick={() => downloadFeature(of)}><Icon name="download" size={20} /> {label} ({mb(st.size)})</button>
    {:else}
      <div class="fd-head">
        {#if job.state === 'downloading' || job.state === 'queued'}<span class="spinner" aria-hidden="true"></span>{/if}
        <span class="fd-text">
          {#if job.state === 'failed'}
            <span class="error">Download stopped: {job.error}</span>
          {:else if job.state === 'paused'}
            Paused at {mb(job.got)} of {mb(job.total)}
          {:else if job.state === 'queued'}
            Waiting for the other downloads…
          {:else}
            Downloading {FEATURE_INFO[of].title.toLowerCase()}… {mb(job.got)} of {mb(job.total)}
          {/if}
        </span>
        {#if job.state === 'paused' || job.state === 'failed'}
          <button class="btn" onclick={() => downloadFeature(of)}>{job.state === 'failed' ? 'Try again' : 'Resume'}</button>
        {:else}
          <button class="btn" onclick={() => pauseFeature(of)}><Icon name="pause" size={18} /> Pause</button>
        {/if}
        <button class="btn" onclick={() => cancelFeature(of)}>Cancel</button>
      </div>
      <div class="fd-track" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={pct} aria-label={FEATURE_INFO[of].title}>
        <i style:width="{pct}%"></i>
      </div>
    {/if}
  </div>
{/if}

<style>
  .fd {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    width: 100%;
    max-width: 460px;
    margin: 12px auto 0;
  }

  .fd.compact {
    align-items: stretch;
    margin: 8px 0 0;
    max-width: none;
  }

  /* In a one-line note: a plain button on the right, progress in a narrow column. */
  .fd.inline {
    flex: none;
    align-items: stretch;
    width: auto;
    margin: 0;
  }

  .fd.inline:has(.fd-head) {
    width: 360px;
    max-width: 50%;
    gap: 6px;
  }

  .fd.inline .fd-head {
    flex-wrap: nowrap;
  }

  .fd.inline .fd-text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .fd-head {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    justify-content: center;
    gap: 8px;
    width: 100%;
  }

  .fd-text {
    flex: 1;
    min-width: 160px;
    font-size: 13px;
  }

  .fd-track {
    width: 100%;
    height: 4px;
    border-radius: 999px;
    background: var(--bg-hover);
    overflow: hidden;
  }

  .fd-track i {
    display: block;
    height: 100%;
    background: var(--accent);
    transition: width 0.4s linear;
  }
</style>
