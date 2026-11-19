<script lang="ts">
  import { untrack } from 'svelte'
  import { api, type AiStatus } from '../lib/api'
  import Icon from '../lib/Icon.svelte'
  import FeatureDownload from './FeatureDownload.svelte'
  import { library } from '../lib/library.svelte'

  /**
   * What face recognition is up to, for the People pages: an empty-page message, or with
   * `compact` (people already showing) just a progress line while it works.
   */
  let { compact = false }: { compact?: boolean } = $props()

  let ai = $state<AiStatus | null | undefined>(undefined)

  function load() {
    api.ai
      .status()
      .then((s) => (ai = s))
      .catch(() => (ai = null))
  }

  // Reload as the library reports progress (throttled by the app).
  $effect(() => {
    library.status?.aiPending
    library.status?.pending
    untrack(load)
  })

  const pending = $derived(library.status?.aiPending ?? ai?.pending ?? 0)
</script>

{#if ai}
  {#if !ai.enabled || ai.state === 'off'}
    {#if !compact}
      <div class="empty">
        <div class="empty-icon"><Icon name="people" size={56} /></div>
        <h2>Face grouping is off</h2>
        <p>Turn it on in Settings to group your photos by the people in them. It runs on this PC only.</p>
        <a class="btn" href="#/settings">Open Settings</a>
      </div>
    {/if}
  {:else if !ai.installed.faces}
    <div class={compact ? 'notice' : 'empty'}>
      {#if !compact}<div class="empty-icon"><Icon name="people" size={56} /></div>{/if}
      <h2>Find the people in your photos</h2>
      <p>Download “People and faces” to group your photos by who is in them. It runs on this PC only.</p>
      <FeatureDownload of="faces" compact={compact} />
    </div>
  {:else if ai.state === 'error'}
    <p class="error">Face recognition could not start: {ai.error}</p>
  {:else if ai.state === 'waiting' || (ai.state === 'working' && pending > 0) || (ai.state === 'starting' && pending > 0)}
    {#if compact}
      <p class="notice-line muted"><span class="spinner"></span> Looking for faces… {pending.toLocaleString()} photos left</p>
    {:else}
      <div class="empty">
        <div class="empty-icon"><Icon name="people" size={56} /></div>
        <h2>Looking for faces…</h2>
        <p>
          {#if ai.state === 'waiting'}
            This starts once your library is indexed.
          {:else}
            {pending.toLocaleString()} photos left. People show up here as they are found.
          {/if}
        </p>
      </div>
    {/if}
  {:else if !compact}
    <div class="empty">
      <div class="empty-icon"><Icon name="people" size={56} /></div>
      <h2>No people yet</h2>
      <p>Someone appears here once their face has been found in at least three photos.</p>
    </div>
  {/if}
{/if}
