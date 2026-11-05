<script lang="ts">
  import { untrack } from 'svelte'
  import { api, countryName, errorMessage, mediaUrl, type Place } from '../lib/api'
  import Icon from '../lib/Icon.svelte'
  import { isBusy, library } from '../lib/library.svelte'

  let places = $state<Place[] | null>(null)
  let error = $state('')

  async function load() {
    try {
      places = await api.places.list()
      error = ''
    } catch (e) {
      error = errorMessage(e)
    }
  }

  $effect(() => {
    load()
  })

  // New places appear while photos are being analyzed.
  let seen: number | null = null
  $effect(() => {
    const revision = library.status?.revision ?? null
    untrack(() => {
      if (seen !== null && revision !== seen) load()
      seen = revision
    })
  })

  /** Places grouped by country, countries by name. */
  const countries = $derived.by(() => {
    const byCountry = new Map<string, Place[]>()
    for (const p of places ?? []) byCountry.set(p.country, [...(byCountry.get(p.country) ?? []), p])
    return [...byCountry]
      .map(([code, list]) => ({ code, name: countryName(code), list }))
      .sort((a, b) => a.name.localeCompare(b.name))
  })

  const busy = $derived(isBusy(library.status))
</script>

<section class="page">
  <h1>Places</h1>

  {#if error}
    <p class="error">{error}</p>
  {:else if places === null}
    <!-- loading -->
  {:else if places.length === 0}
    <div class="empty">
      <div class="empty-icon"><Icon name="explore" size={56} /></div>
      {#if busy}
        <h2>Looking up places…</h2>
        <p>Photos with location data are being sorted by city. This page fills in as it goes.</p>
      {:else}
        <h2>No places yet</h2>
        <p>Photos and videos that have GPS location data are grouped by city here. No internet is needed.</p>
      {/if}
    </div>
  {:else}
    {#each countries as country (country.code)}
      <h2 class="section-title">{country.name}</h2>
      <div class="album-grid">
        {#each country.list as place (place.id)}
          <a class="album-card" href={`#/place/${place.id}`}>
            <span class="album-cover">
              {#if place.coverId !== null}
                <img
                  src={mediaUrl(place.coverId, 'thumb', place.coverM ?? undefined)}
                  alt=""
                  draggable="false"
                  loading="lazy"
                />
              {:else}
                <Icon name="explore" size={48} />
              {/if}
            </span>
            <span class="album-name">{place.city}</span>
            <span class="muted">
              {place.admin1 ? `${place.admin1} · ` : ''}{place.count.toLocaleString()}
              {place.count === 1 ? 'item' : 'items'}
            </span>
          </a>
        {/each}
      </div>
    {/each}
  {/if}
</section>
