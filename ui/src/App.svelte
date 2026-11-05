<script lang="ts">
  import { untrack } from 'svelte'
  import { api } from './lib/api'
  import Dialogs from './components/Dialogs.svelte'
  import Toasts from './components/Toasts.svelte'
  import Icon from './lib/Icon.svelte'
  import type { IconName } from './lib/icons'
  import { isBusy, library, watchLibrary } from './lib/library.svelte'
  import { goSearch, router, type Route } from './lib/router.svelte'
  import { applyTheme } from './lib/theme'
  import AlbumView from './pages/AlbumView.svelte'
  import Albums from './pages/Albums.svelte'
  import Duplicates from './pages/Duplicates.svelte'
  import Explore from './pages/Explore.svelte'
  import Favorites from './pages/Favorites.svelte'
  import MapView from './pages/MapView.svelte'
  import Photos from './pages/Photos.svelte'
  import PlaceView from './pages/PlaceView.svelte'
  import Places from './pages/Places.svelte'
  import Search from './pages/Search.svelte'
  import Settings from './pages/Settings.svelte'
  import Trash from './pages/Trash.svelte'
  import Utilities from './pages/Utilities.svelte'

  const nav: { route: Route; label: string; icon: IconName }[] = [
    { route: 'photos', label: 'Photos', icon: 'photo' },
    { route: 'explore', label: 'Explore', icon: 'explore' },
    { route: 'favorites', label: 'Favorites', icon: 'star' },
    { route: 'albums', label: 'Albums', icon: 'albums' },
    { route: 'utilities', label: 'Utilities', icon: 'utilities' },
    { route: 'trash', label: 'Trash', icon: 'trash' },
  ]

  watchLibrary()

  /** Routes whose content is a photo grid, which fills the whole content area. */
  const FILL: Route[] = ['photos', 'favorites', 'trash', 'album', 'place', 'map', 'search']

  /** Sidebar entry that stays highlighted on a page. */
  const sidebarFor: Partial<Record<Route, Route>> = {
    album: 'albums',
    places: 'explore',
    place: 'explore',
    map: 'explore',
    duplicates: 'utilities',
  }

  // ---------- search box ----------

  let searchText = $state('')
  let searchTimer: ReturnType<typeof setTimeout> | undefined

  // Show the query of the search page in the box (also after Back/Forward).
  $effect(() => {
    if (router.route !== 'search') return
    const q = router.params.get('q') ?? ''
    // Don't touch what is being typed (e.g. a trailing space) when the URL just caught up.
    if (untrack(() => searchText.trim()) !== q) searchText = q
  })

  function runSearch(text: string, replace: boolean) {
    clearTimeout(searchTimer)
    const keep = router.route === 'search' ? Object.fromEntries(router.params) : {}
    goSearch({ ...keep, q: text.trim() }, replace && router.route === 'search')
  }

  function onSearchInput() {
    clearTimeout(searchTimer)
    if (!searchText.trim() && router.route !== 'search') return
    searchTimer = setTimeout(() => {
      // The user may have left the search page (or cleared the box) in the meantime.
      if (!searchText.trim() && router.route !== 'search') return
      runSearch(searchText, true)
    }, 350)
  }

  function onSearchKey(e: KeyboardEvent) {
    if (e.key === 'Enter') runSearch(searchText, true)
    else if (e.key === 'Escape') {
      searchText = ''
      clearTimeout(searchTimer)
      if (router.route === 'search') runSearch('', true)
      ;(e.currentTarget as HTMLInputElement).blur()
    }
  }

  const progress = $derived.by(() => {
    const s = library.status
    if (!isBusy(s)) return null
    if (s!.scanning && s!.scannedFiles > 0) return `Scanning… ${s!.scannedFiles.toLocaleString()} found`
    if (s!.pending > 0) return `Indexing… ${s!.pending.toLocaleString()} left`
    return 'Scanning…'
  })

  // Apply the saved theme on startup.
  $effect(() => {
    api.settings
      .get()
      .then((s) => applyTheme(s.theme))
      .catch(() => {})
  })
</script>

<div class="shell">
  <header class="topbar">
    <a class="brand" href="#/photos">
      <img src="/favicon.svg" alt="" width="28" height="28" />
      <span>Memora</span>
    </a>
    <label class="search">
      <Icon name="search" size={22} />
      <input
        type="search"
        placeholder="Search your photos"
        bind:value={searchText}
        oninput={onSearchInput}
        onkeydown={onSearchKey}
        onfocus={(e) => e.currentTarget.select()}
      />
    </label>
    {#if progress}
      <a class="progress-chip" href="#/settings" title={library.status?.scanFolder ?? 'Indexing your library'}>
        <span class="spinner"></span>
        {progress}
      </a>
    {/if}
    <a class="icon-btn" class:active={router.route === 'settings'} href="#/settings" title="Settings">
      <Icon name="settings" />
    </a>
  </header>

  <nav class="sidebar">
    {#each nav as item (item.route)}
      <a class="nav-item" class:active={(sidebarFor[router.route] ?? router.route) === item.route} href={`#/${item.route}`}>
        <Icon name={item.icon} size={22} />
        <span>{item.label}</span>
      </a>
    {/each}
  </nav>

  <main class="content" class:fill={FILL.includes(router.route)}>
    {#if router.route === 'photos'}
      <Photos />
    {:else if router.route === 'favorites'}
      <Favorites />
    {:else if router.route === 'albums'}
      <Albums />
    {:else if router.route === 'album' && router.id !== null}
      <AlbumView id={router.id} />
    {:else if router.route === 'trash'}
      <Trash />
    {:else if router.route === 'search'}
      <Search />
    {:else if router.route === 'explore'}
      <Explore />
    {:else if router.route === 'places'}
      <Places />
    {:else if router.route === 'place' && router.id !== null}
      <PlaceView id={router.id} />
    {:else if router.route === 'map'}
      <MapView />
    {:else if router.route === 'utilities'}
      <Utilities />
    {:else if router.route === 'duplicates'}
      <Duplicates />
    {:else if router.route === 'settings'}
      <Settings />
    {/if}
  </main>
</div>

<Dialogs />
<Toasts />
