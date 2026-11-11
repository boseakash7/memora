<script lang="ts">
  import { untrack } from 'svelte'
  import AiNotice from '../components/AiNotice.svelte'
  import { api, errorMessage, faceUrl, type Person } from '../lib/api'
  import Icon from '../lib/Icon.svelte'
  import { library } from '../lib/library.svelte'

  let people = $state<Person[] | null>(null)
  let showHidden = $state(false)
  let error = $state('')

  async function load() {
    try {
      people = await api.people.list(showHidden)
      error = ''
    } catch (e) {
      error = errorMessage(e)
    }
  }

  $effect(() => {
    showHidden
    untrack(load)
  })

  // New people appear while photos are being looked at.
  let seen: number | null = null
  $effect(() => {
    const revision = library.status?.revision ?? null
    untrack(() => {
      if (seen !== null && revision !== seen) load()
      seen = revision
    })
  })

  const anyHidden = $derived(showHidden && (people ?? []).some((p) => p.hidden))
</script>

<section class="page">
  <div class="page-head">
    <h1>People</h1>
    <label class="check">
      <input type="checkbox" bind:checked={showHidden} />
      <span>Show hidden people</span>
    </label>
  </div>

  <AiNotice compact={(people?.length ?? 0) > 0} />

  {#if error}
    <p class="error">{error}</p>
  {:else if people !== null && people.length > 0}
    <div class="people-grid">
      {#each people as p (p.id)}
        <a class="person-card" class:dim={p.hidden} href={`#/person/${p.id}`}>
          <span class="person-face">
            {#if p.coverFace !== null}
              <img src={faceUrl(p.coverFace)} alt="" draggable="false" loading="lazy" />
            {:else}
              <Icon name="person" size={48} />
            {/if}
            {#if p.hidden}<span class="person-badge" title="Hidden"><Icon name="visibilityOff" size={16} /></span>{/if}
          </span>
          <span class="person-name" class:muted={!p.name}>{p.name ?? 'Add a name'}</span>
          <span class="muted small">{p.count.toLocaleString()} {p.count === 1 ? 'item' : 'items'}</span>
        </a>
      {/each}
    </div>
    {#if anyHidden}
      <p class="muted small pad">Hidden people don't show in People or Explore. Their photos stay in your library.</p>
    {/if}
  {/if}
</section>
