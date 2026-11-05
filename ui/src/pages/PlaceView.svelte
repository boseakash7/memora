<script lang="ts">
  import { untrack } from 'svelte'
  import MediaPage from '../components/MediaPage.svelte'
  import { api, countryName, type Place } from '../lib/api'
  import Icon from '../lib/Icon.svelte'
  import { library } from '../lib/library.svelte'
  import { go } from '../lib/router.svelte'

  let { id }: { id: number } = $props()

  let place = $state<Place | null>(null)

  async function load() {
    try {
      place = await api.places.get(id)
    } catch {
      go('places') // no photos there any more
    }
  }

  $effect(() => {
    id
    untrack(load)
  })

  let seen: number | null = null
  $effect(() => {
    const revision = library.status?.revision ?? null
    untrack(() => {
      if (seen !== null && revision !== seen) load()
      seen = revision
    })
  })

  const subtitle = $derived(
    place
      ? [place.admin1, countryName(place.country)].filter(Boolean).join(', ') +
          ` · ${place.count.toLocaleString()} ${place.count === 1 ? 'item' : 'items'}`
      : '',
  )
</script>

{#key id}
  <MediaPage query={{ place: id }} title={place?.city ?? 'Place'} {subtitle}>
    {#snippet empty()}
      <div class="empty">
        <div class="empty-icon"><Icon name="explore" size={56} /></div>
        <h2>No photos here</h2>
        <p>Nothing is left at this place.</p>
        <button class="btn" onclick={() => go('places')}>All places</button>
      </div>
    {/snippet}
  </MediaPage>
{/key}
