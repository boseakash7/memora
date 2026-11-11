<script lang="ts">
  import MediaPage from '../components/MediaPage.svelte'
  import { api, type TimelineQuery } from '../lib/api'
  import Icon from '../lib/Icon.svelte'
  import { goSearch, router } from '../lib/router.svelte'

  let cameras = $state<string[]>([])

  $effect(() => {
    api
      .cameras()
      .then((c) => (cameras = c))
      .catch(() => {})
  })

  const q = $derived(router.params.get('q') ?? '')
  const kind = $derived(router.params.get('kind') ?? '')
  const camera = $derived(router.params.get('camera') ?? '')
  const from = $derived(router.params.get('from') ?? '')
  const to = $derived(router.params.get('to') ?? '')

  const hasQuery = $derived(!!(q.trim() || kind || camera || from || to))

  const query = $derived<TimelineQuery>({
    q: q.trim() || undefined,
    kind: kind === 'photo' || kind === 'video' ? kind : undefined,
    camera: camera || undefined,
    from: from || undefined,
    to: to || undefined,
  })

  function set(key: 'kind' | 'camera' | 'from' | 'to', value: string) {
    goSearch({ q, kind, camera, from, to, [key]: value }, true)
  }

  const filtered = $derived(!!(kind || camera || from || to))
</script>

<div class="media-page">
  <div class="filter-bar">
    <span class="filter-label"><Icon name="filter" size={20} /></span>
    <label>
      <span class="muted">Type</span>
      <select value={kind} onchange={(e) => set('kind', e.currentTarget.value)}>
        <option value="">Photos and videos</option>
        <option value="photo">Photos</option>
        <option value="video">Videos</option>
      </select>
    </label>
    {#if cameras.length > 0 || camera}
      <label>
        <span class="muted">Camera</span>
        <select value={camera} onchange={(e) => set('camera', e.currentTarget.value)}>
          <option value="">Any camera</option>
          {#each cameras as c (c)}
            <option value={c}>{c}</option>
          {/each}
          {#if camera && !cameras.includes(camera)}<option value={camera}>{camera}</option>{/if}
        </select>
      </label>
    {/if}
    <label>
      <span class="muted">From</span>
      <input type="date" value={from} max={to || undefined} onchange={(e) => set('from', e.currentTarget.value)} />
    </label>
    <label>
      <span class="muted">To</span>
      <input type="date" value={to} min={from || undefined} onchange={(e) => set('to', e.currentTarget.value)} />
    </label>
    {#if filtered}
      <button class="btn" onclick={() => goSearch({ q }, true)}>Clear filters</button>
    {/if}
  </div>

  <div class="grid-host">
    {#if hasQuery}
      <MediaPage {query}>
          {#snippet empty()}
            <div class="empty">
              <div class="empty-icon"><Icon name="search" size={56} /></div>
              <h2>No results</h2>
              <p>
                Nothing matches{q.trim() ? ` “${q.trim()}”` : ''}. Search looks at what is in your photos, people's
                names, places, file and folder names, cameras and albums.
              </p>
            </div>
          {/snippet}
      </MediaPage>
    {:else}
      <section class="page">
        <div class="empty">
          <div class="empty-icon"><Icon name="search" size={56} /></div>
          <h2>Search your photos</h2>
          <p>
            Describe what you're looking for (“sunset”, “dog on a beach”), or type a person's name, a place, a
            file or folder name, a camera or an album. You can also pick a date range above.
          </p>
        </div>
      </section>
    {/if}
  </div>
</div>
