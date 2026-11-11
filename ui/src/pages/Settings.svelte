<script lang="ts">
  import { ask, open } from '@tauri-apps/plugin-dialog'
  import {
    api,
    errorMessage,
    serverInfo,
    showInExplorer,
    type Folder,
    type ServerInfo,
    type Settings,
    type Theme,
  } from '../lib/api'
  import Icon from '../lib/Icon.svelte'
  import AiCard from '../components/AiCard.svelte'
  import SharingCard from '../components/SharingCard.svelte'
  import { isBusy, library } from '../lib/library.svelte'
  import { applyTheme } from '../lib/theme'

  let folders = $state<Folder[]>([])
  let settings = $state<Settings | null>(null)
  let info = $state<ServerInfo | null>(null)
  let folderError = $state('')
  let error = $state('')
  let busy = $state(false)

  const themes: { value: Theme; label: string }[] = [
    { value: 'system', label: 'Use Windows setting' },
    { value: 'light', label: 'Light' },
    { value: 'dark', label: 'Dark' },
  ]

  $effect(() => {
    Promise.all([api.folders.list(), api.settings.get(), serverInfo()])
      .then(([f, s, i]) => {
        folders = f
        settings = s
        info = i
      })
      .catch((e) => (error = errorMessage(e)))
  })

  async function addFolder() {
    folderError = ''
    const picked = await open({ directory: true, multiple: false, title: 'Choose a folder with photos or videos' })
    if (typeof picked !== 'string') return
    busy = true
    try {
      const folder = await api.folders.add(picked)
      folders = [...folders, folder].sort((a, b) => a.path.localeCompare(b.path))
    } catch (e) {
      folderError = errorMessage(e)
    } finally {
      busy = false
    }
  }

  async function removeFolder(folder: Folder) {
    folderError = ''
    const ok = await ask(
      `Remove "${folder.path}" from the library?\n\nYour files are not deleted; they just stop showing in Memora.`,
      { title: 'Remove folder', kind: 'warning', okLabel: 'Remove', cancelLabel: 'Cancel' },
    )
    if (!ok) return
    try {
      await api.folders.remove(folder.id)
      folders = folders.filter((f) => f.id !== folder.id)
    } catch (e) {
      folderError = errorMessage(e)
    }
  }

  async function setTheme(theme: Theme) {
    applyTheme(theme)
    try {
      settings = await api.settings.update({ theme })
    } catch (e) {
      error = errorMessage(e)
    }
  }

  async function rescan() {
    folderError = ''
    try {
      await api.library.rescan()
    } catch (e) {
      folderError = errorMessage(e)
    }
  }

  const status = $derived(library.status)

  async function open_(path: string) {
    try {
      await showInExplorer(path)
    } catch (e) {
      folderError = errorMessage(e)
    }
  }
</script>

<section class="page narrow">
  <h1>Settings</h1>
  {#if error}<p class="error">{error}</p>{/if}

  <div class="card">
    <div class="card-head">
      <div>
        <h2>Library folders</h2>
        <p class="muted">Photos and videos in these folders (and their subfolders) appear in your library.</p>
      </div>
      <button class="btn primary" onclick={addFolder} disabled={busy}>
        <Icon name="add" size={20} /> Add folder
      </button>
    </div>

    {#if folderError}<p class="error">{folderError}</p>{/if}

    {#if folders.length === 0}
      <p class="muted pad">No folders yet. Add one to get started.</p>
    {:else}
      <ul class="folder-list">
        {#each folders as folder (folder.id)}
          <li>
            <span class="folder-icon"><Icon name="folder" size={22} /></span>
            <span class="folder-path" title={folder.path}>{folder.path}</span>
            {#if !folder.available}
              <span class="badge warn" title="The folder can't be reached right now (unplugged drive?)">
                <Icon name="warning" size={14} /> Not available
              </span>
            {/if}
            <button class="icon-btn" title="Show in Explorer" onclick={() => open_(folder.path)}>
              <Icon name="openExternal" size={20} />
            </button>
            <button class="icon-btn" title="Remove from library" onclick={() => removeFolder(folder)}>
              <Icon name="close" size={20} />
            </button>
          </li>
        {/each}
      </ul>
    {/if}

    {#if status && folders.length > 0}
      <div class="library-status">
        {#if isBusy(status)}
          <span class="spinner"></span>
          <span>
            {#if status.scanning}
              Scanning{status.scanFolder ? ` ${status.scanFolder}` : ''}… {status.scannedFiles.toLocaleString()} found
            {:else}
              Indexing… {status.pending.toLocaleString()} left
            {/if}
          </span>
        {:else}
          <span>
            {status.total.toLocaleString()} photos and videos.
            {#if status.errors > 0}<span class="muted">{status.errors.toLocaleString()} couldn't be read.</span>{/if}
          </span>
        {/if}
        <button class="btn" onclick={rescan} disabled={status.scanning} title="Look for new, changed and deleted files">
          <Icon name="refresh" size={20} /> Rescan
        </button>
      </div>
    {/if}
  </div>

  {#if settings}
    <div class="card">
      <h2>Appearance</h2>
      <div class="radio-group" role="radiogroup" aria-label="Theme">
        {#each themes as t (t.value)}
          <label class="radio">
            <input
              type="radio"
              name="theme"
              value={t.value}
              checked={settings.theme === t.value}
              onchange={() => setTheme(t.value)}
            />
            {t.label}
          </label>
        {/each}
      </div>
    </div>
  {/if}

  {#if settings}
    <AiCard {settings} onchange={(s) => (settings = s)} />
    <SharingCard {settings} onchange={(s) => (settings = s)} />
  {/if}

  {#if info}
    <div class="card">
      <h2>About</h2>
      <p class="muted">Memora {info.version}. Works offline; nothing leaves this PC.</p>
      <p class="muted">
        App data (database and thumbnails):
        <button class="link" onclick={() => open_(info!.dataDir)}>{info.dataDir}</button>
      </p>
    </div>
  {/if}
</section>
