<script lang="ts">
  import { api, errorMessage, faceUrl, type Person } from '../lib/api'
  import Icon from '../lib/Icon.svelte'
  import { toast } from '../lib/toast.svelte'

  /** Choose a person, e.g. to merge with. Named people first, then by number of photos. */
  let {
    title,
    exclude = null,
    onpick,
    onclose,
  }: {
    title: string
    exclude?: number | null
    onpick: (p: Person) => void
    onclose: () => void
  } = $props()

  let people = $state<Person[] | null>(null)
  let filter = $state('')

  $effect(() => {
    api.people
      .list(true)
      .then((list) => {
        people = list
          .filter((p) => p.id !== exclude)
          .sort((a, b) => Number(!a.name) - Number(!b.name) || b.count - a.count)
      })
      .catch((e) => {
        toast(errorMessage(e), { error: true })
        onclose()
      })
  })

  const shown = $derived.by(() => {
    const f = filter.trim().toLowerCase()
    return (people ?? []).filter((p) => !f || (p.name ?? '').toLowerCase().includes(f))
  })

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault()
      e.stopPropagation()
      onclose()
    }
  }
</script>

<svelte:window onkeydowncapture={onkeydown} />

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="modal-backdrop" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div class="modal picker" role="dialog" aria-modal="true" aria-label={title}>
    <h2>{title}</h2>
    <label class="field">
      <!-- svelte-ignore a11y_autofocus -->
      <input type="search" placeholder="Search names" bind:value={filter} autofocus />
    </label>
    <div class="picker-list">
      {#if people === null}
        <div class="pad"><span class="spinner"></span></div>
      {:else if shown.length === 0}
        <p class="muted pad">Nobody to choose.</p>
      {:else}
        {#each shown as p (p.id)}
          <button class="picker-row" onclick={() => onpick(p)}>
            <span class="picker-thumb round">
              {#if p.coverFace !== null}
                <img src={faceUrl(p.coverFace)} alt="" draggable="false" loading="lazy" />
              {:else}
                <Icon name="person" size={24} />
              {/if}
            </span>
            <span class="picker-name" class:muted={!p.name}>{p.name ?? 'Unnamed'}</span>
            <span class="muted">{p.count}</span>
          </button>
        {/each}
      {/if}
    </div>
    <div class="modal-actions">
      <button class="btn" onclick={onclose}>Cancel</button>
    </div>
  </div>
</div>
