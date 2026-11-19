<script lang="ts">
  import { untrack } from 'svelte'
  import { api } from './lib/api'
  import ContextMenu from './components/ContextMenu.svelte'
  import Dialogs from './components/Dialogs.svelte'
  import Toasts from './components/Toasts.svelte'
  import Welcome from './components/Welcome.svelte'
  import WindowControls from './components/WindowControls.svelte'
  import Icon from './lib/Icon.svelte'
  import type { IconName } from './lib/icons'
  import { formatBytes } from './lib/dates'
  import { isBusy, library, watchLibrary, indexingStep } from './lib/library.svelte'
  import { goSearch, router, type Route } from './lib/router.svelte'
  import { applyTheme } from './lib/theme'
  import { applyAnimations } from './lib/motion.svelte'
  import { appWindow, inTauri, showSystemMenu, titleBarContextMenu } from './lib/window.svelte'
  import AlbumView from './pages/AlbumView.svelte'
  import Albums from './pages/Albums.svelte'
  import Duplicates from './pages/Duplicates.svelte'
  import Explore from './pages/Explore.svelte'
  import Favorites from './pages/Favorites.svelte'
  import MapView from './pages/MapView.svelte'
  import Photos from './pages/Photos.svelte'
  import PlaceView from './pages/PlaceView.svelte'
  import Places from './pages/Places.svelte'
  import People from './pages/People.svelte'
  import PersonView from './pages/PersonView.svelte'
  import Search from './pages/Search.svelte'
  import Settings from './pages/Settings.svelte'
  import Trash from './pages/Trash.svelte'
  import Archive from './pages/Archive.svelte'
  import Utilities from './pages/Utilities.svelte'
  import Memories from './pages/Memories.svelte'
  import Vault from './pages/Vault.svelte'

  const nav: { route: Route; label: string; icon: IconName }[] = [
    { route: 'photos', label: 'Photos', icon: 'photo' },
    { route: 'memories', label: 'Memories', icon: 'memories' },
    { route: 'explore', label: 'Explore', icon: 'explore' },
    { route: 'favorites', label: 'Favorites', icon: 'star' },
    { route: 'archive', label: 'Archive', icon: 'archive' },
    { route: 'albums', label: 'Albums', icon: 'albums' },
    { route: 'utilities', label: 'Utilities', icon: 'utilities' },
    { route: 'trash', label: 'Trash', icon: 'trash' },
  ]

  watchLibrary()

  /** Routes whose content is a photo grid, which fills the whole content area. */
  const FILL: Route[] = ['photos', 'favorites', 'archive', 'trash', 'album', 'place', 'person', 'map', 'search', 'vault']

  /** Sidebar entry that stays highlighted on a page. */
  const sidebarFor: Partial<Record<Route, Route>> = {
    album: 'albums',
    places: 'explore',
    place: 'explore',
    map: 'explore',
    people: 'explore',
    person: 'explore',
    duplicates: 'utilities',
    vault: 'utilities',
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

  const plural = (n: number, one: string, many: string) => `${n.toLocaleString()} ${n === 1 ? one : many}`

  const progress = $derived.by(() => {
    const s = library.status
    if (!isBusy(s)) return null
    if (s!.scanning && s!.scannedFiles > 0) return `Scanning… ${s!.scannedFiles.toLocaleString()} found`
    if (s!.pending > 0) return indexingStep(s!)
    return 'Scanning…'
  })

  // Alt+Space opens the window menu, as on a native title bar.
  function onWindowKey(e: KeyboardEvent) {
    if (e.altKey && e.code === 'Space' && !e.ctrlKey && !e.shiftKey) {
      e.preventDefault()
      showSystemMenu(0, 48)
    }
  }

  // The webview's own menu (Back, Reload, Save image as, Inspect…) is off, except where Cut/Copy/
  // Paste are useful: in text fields and on selected text. Our menus call preventDefault first.
  function onDefaultContextMenu(e: MouseEvent) {
    if (e.defaultPrevented) return
    const target = e.target as Element | null
    if (target?.closest('input, textarea, [contenteditable]:not([contenteditable="false"])')) return
    const sel = window.getSelection()
    if (sel && !sel.isCollapsed && target && sel.containsNode(target, true)) return
    e.preventDefault()
  }

  // Dim the title bar while another window is active, like native windows.
  $effect(() => {
    document.documentElement.classList.toggle('window-inactive', !appWindow.focused)
  })

  let welcome = $state(false)

  // Apply the saved theme and animation level on startup, and greet a new user. A library
  // that already has folders (set up before the welcome existed) skips it.
  $effect(() => {
    api.settings
      .get()
      .then(async (s) => {
        applyTheme(s.theme)
        applyAnimations(s.animations)
        if (s.welcomeDone) return
        if ((await api.folders.list()).length > 0) await api.settings.update({ welcomeDone: true })
        else welcome = true
      })
      .catch(() => {})
  })
</script>

<svelte:window onkeydown={onWindowKey} oncontextmenu={onDefaultContextMenu} />

<div class="shell">
  <!-- The top bar is the window's title bar: its empty parts move the window. -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <header class="topbar" data-tauri-drag-region="deep" oncontextmenu={titleBarContextMenu}>
    <a class="brand" href="#/photos">
      <img src="/favicon.svg" alt="" width="28" height="28" />
      <span>Memora</span>
    </a>
    <label class="search">
      <Icon name="search" size={22} />
      <input
        type="search"
        placeholder="Search people, places, things…"
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
    <div class="nav-list">
      {#each nav as item (item.route)}
        <a
          class="nav-item"
          class:active={(sidebarFor[router.route] ?? router.route) === item.route}
          href={`#/${item.route}`}
        >
          <Icon name={item.icon} size={22} />
          <span>{item.label}</span>
        </a>
      {/each}
    </div>
    {#if library.status}
      {@const s = library.status}
      <a class="library-summary" href="#/settings" title="Library settings">
        <span>{plural(s.photos, 'photo', 'photos')} · {plural(s.videos, 'video', 'videos')}</span>
        <span class="muted">{formatBytes(s.bytes)} on disk</span>
      </a>
    {/if}
  </nav>

  <main class="content" class:fill={FILL.includes(router.route)}>
    {#if router.route === 'photos'}
      <Photos />
    {:else if router.route === 'favorites'}
      <Favorites />
    {:else if router.route === 'archive'}
      <Archive />
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
    {:else if router.route === 'people'}
      <People />
    {:else if router.route === 'person' && router.id !== null}
      <PersonView id={router.id} />
    {:else if router.route === 'map'}
      <MapView />
    {:else if router.route === 'utilities'}
      <Utilities />
    {:else if router.route === 'duplicates'}
      <Duplicates />
    {:else if router.route === 'vault'}
      <Vault />
    {:else if router.route === 'memories'}
      <Memories />
    {:else if router.route === 'settings'}
      <Settings />
    {/if}
  </main>
</div>

{#if welcome}<Welcome ondone={() => (welcome = false)} />{/if}
<Dialogs />
<Toasts />
<ContextMenu />
{#if inTauri}<WindowControls />{/if}
