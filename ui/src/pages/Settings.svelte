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
  import StorageCard from '../components/StorageCard.svelte'
  import BackupCard from '../components/BackupCard.svelte'
  import { indexingStep, isBusy, library } from '../lib/library.svelte'
  import { applyTheme } from '../lib/theme'
  import { formatBytes } from '../lib/dates'

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
      const keepInfo = await askKeepInfo(picked)
      const folder = await api.folders.add(picked, keepInfo)
      folders = [...folders, folder].sort((a, b) => a.path.localeCompare(b.path))
    } catch (e) {
      folderError = errorMessage(e)
    } finally {
      busy = false
    }
  }

  /** Ask whether the folder should carry its library info (`.memora`) when it is added. */
  async function askKeepInfo(path: string): Promise<boolean> {
    const probe = await api.folders.probe(path)
    if (probe.root) {
      const from = probe.pcs.length ? probe.pcs.join(', ') : 'another PC'
      return ask(
        `This folder already has library info from ${from}: favorites, albums, the Archive, the trash and people's names.\n\n` +
          'Use it here, and keep the changes you make on this PC there too?',
        { title: 'Library info found', kind: 'info', okLabel: 'Use it', cancelLabel: 'Only on this PC' },
      )
    }
    return ask(
      'Keep your library info in this folder too?\n\n' +
        `Memora adds a small hidden ".memora" folder with your favorites, albums, Archive, trash and people's names. ` +
        'Another PC that adds this folder (external drive, NAS or a synced folder) then shows the same library and stays in step.\n\n' +
        'Your photos themselves are never changed.',
      { title: 'Library info', kind: 'info', okLabel: 'Keep it in the folder', cancelLabel: 'Only on this PC' },
    )
  }

  async function toggleKeepInfo(folder: Folder) {
    folderError = ''
    try {
      let updated: Folder
      if (folder.sync) {
        const ok = await ask(
          `Stop keeping library info in "${folder.syncRoot ?? folder.path}"?\n\n` +
            'What is there now stays, so other PCs can keep using it. Changes you make here stay on this PC.',
          { title: 'Library info', kind: 'warning', okLabel: 'Stop', cancelLabel: 'Cancel' },
        )
        if (!ok) return
        updated = await api.folders.setKeepInfo(folder.id, false)
        if (folder.syncRoot === folder.path) {
          const remove = await ask(
            `Also move the folder's ".memora" to the Recycle Bin?\n\nOther PCs that use this folder lose the library info kept there.`,
            { title: 'Library info', kind: 'warning', okLabel: 'Move to Recycle Bin', cancelLabel: 'Keep it' },
          )
          if (remove) updated = await api.folders.removeInfo(folder.id)
        }
      } else {
        updated = await api.folders.setKeepInfo(folder.id, true)
      }
      folders = folders.map((f) => (f.id === updated.id ? updated : f))
    } catch (e) {
      folderError = errorMessage(e)
    }
  }

  async function setKeepIndex(folder: Folder, keep: boolean) {
    folderError = ''
    try {
      const updated = await api.folders.setKeepIndex(folder.id, keep)
      folders = folders.map((f) => (f.id === updated.id ? updated : f))
    } catch (e) {
      folderError = errorMessage(e)
    }
  }

  /** Tooltip for a folder's library-info badge. */
  function infoTitle(folder: Folder): string {
    const i = folder.libraryInfo
    if (!i) return ''
    const where = `Library info is kept in ${i.root}\\.memora`
    const others = i.otherPcs.length ? `Shared with ${i.otherPcs.join(', ')}.` : 'No other PC uses it yet.'
    const waiting = i.unsent ? ` ${i.unsent.toLocaleString()} changes wait to be written.` : ''
    return `${where}. ${others}${waiting}`
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

  /** The folder moved or its drive letter changed: point it at the new place. */
  async function locateFolder(folder: Folder) {
    folderError = ''
    const picked = await open({ directory: true, multiple: false, title: `Where is "${folder.path}" now?` })
    if (typeof picked !== 'string') return
    busy = true
    try {
      const located = await api.folders.locate(folder.id, picked)
      folders = folders.map((f) => (f.id === located.id ? located : f)).sort((a, b) => a.path.localeCompare(b.path))
    } catch (e) {
      folderError = errorMessage(e)
    } finally {
      busy = false
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
              <button
                class="btn"
                disabled={busy}
                title="The folder moved or its drive letter changed: choose where it is now"
                onclick={() => locateFolder(folder)}
              >
                Locate…
              </button>
            {/if}
            {#if folder.libraryInfo}
              {#if folder.libraryInfo.writable}
                <span class="badge" title={infoTitle(folder)}>
                  <Icon name="sync" size={14} />
                  {folder.libraryInfo.otherPcs.length ? `Shared with ${folder.libraryInfo.otherPcs.length} PC${folder.libraryInfo.otherPcs.length > 1 ? 's' : ''}` : 'Info in folder'}
                </span>
              {:else if folder.available}
                <span class="badge warn" title={`Memora can't write to ${folder.libraryInfo.root}, so the library info stays on this PC for now. ${infoTitle(folder)}`}>
                  <Icon name="warning" size={14} /> Info kept on this PC
                </span>
              {/if}
            {/if}
            <button
              class="icon-btn"
              class:active={folder.sync}
              title={folder.sync ? 'Stop keeping library info in this folder' : 'Keep library info in this folder, so other PCs that add it show the same library'}
              aria-pressed={folder.sync}
              onclick={() => toggleKeepInfo(folder)}
            >
              <Icon name="sync" size={20} />
            </button>
            <button class="icon-btn" title="Show in Explorer" onclick={() => open_(folder.path)}>
              <Icon name="openExternal" size={20} />
            </button>
            <button class="icon-btn" title="Remove from library" onclick={() => removeFolder(folder)}>
              <Icon name="close" size={20} />
            </button>
          </li>
          {#if folder.sync}
            <li class="folder-sub">
              <label title="Other PCs that add this folder then skip reading, hashing and recognising the photos they have too; only thumbnails are made there">
                <input type="checkbox" checked={folder.keepIndex} onchange={(e) => setKeepIndex(folder, e.currentTarget.checked)} />
                Also keep the finished index there, so other PCs skip indexing
                {#if folder.keepIndex && folder.libraryInfo?.indexBytes}
                  <span class="muted">({formatBytes(folder.libraryInfo.indexBytes)})</span>
                {/if}
              </label>
            </li>
          {/if}
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
              {indexingStep(status)}
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

  <StorageCard />
  <BackupCard />

  {#if info}
    <div class="card">
      <h2>About</h2>
      <p class="muted">Memora {info.version}. Works offline; nothing leaves this PC.</p>
    </div>
  {/if}
</section>
