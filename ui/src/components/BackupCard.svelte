<script lang="ts">
  import { ask, open, save } from '@tauri-apps/plugin-dialog'
  import { api, errorMessage, type Restored } from '../lib/api'
  import { confirmDialog } from '../lib/dialogs.svelte'
  import Icon from '../lib/Icon.svelte'
  import { toast } from '../lib/toast.svelte'

  const filters = [{ name: 'Memora backup', extensions: ['memora-backup'] }]
  let busy = $state(false)
  let error = $state('')
  let restored = $state<Restored | null>(null)

  const n = (count: number, one: string, many: string) => `${count.toLocaleString()} ${count === 1 ? one : many}`

  async function backUp() {
    error = ''
    const day = new Date().toISOString().slice(0, 10)
    const path = await save({ title: 'Save library info', defaultPath: `Memora library ${day}.memora-backup`, filters })
    if (!path) return
    busy = true
    try {
      const s = await api.backup.write(path)
      toast(`Saved ${n(s.favorites, 'favorite', 'favorites')}, ${n(s.albums, 'album', 'albums')} and ${n(s.people, 'name', 'names')}`)
    } catch (e) {
      error = errorMessage(e)
    } finally {
      busy = false
    }
  }

  async function restore() {
    error = ''
    restored = null
    const path = await open({ title: 'Restore library info', multiple: false, directory: false, filters })
    if (typeof path !== 'string') return
    try {
      const s = await api.backup.read(path)
      const when = new Date(s.created).toLocaleString()
      const ok = await confirmDialog({
        title: 'Restore library info?',
        message:
          `From ${s.pc || 'a PC'}, ${when}: ${n(s.favorites, 'favorite', 'favorites')}, ${n(s.albums, 'album', 'albums')}, ` +
          `${n(s.people, 'named person', 'named people')}, ${n(s.archived, 'archived item', 'archived items')} and ` +
          `${n(s.trashed, 'item', 'items')} in the trash, for ${n(s.folders.length, 'folder', 'folders')}. ` +
          'They are added to your library; nothing you changed since is undone. Folders that are not in the library yet are added.',
        confirmLabel: 'Restore',
      })
      if (!ok) return
      const withSettings = await ask('Also restore the settings from the backup (theme, TV sharing, recognition)?', {
        title: 'Restore library info',
        kind: 'info',
        okLabel: 'Restore settings too',
        cancelLabel: 'Keep my settings',
      })
      busy = true
      restored = await api.backup.restore(path, withSettings)
      toast('Library info restored')
    } catch (e) {
      error = errorMessage(e)
    } finally {
      busy = false
    }
  }
</script>

<div class="card">
  <div class="card-head">
    <div>
      <h2>Back up library info</h2>
      <p class="muted">
        Save your favorites, albums, Archive, trash and people's names to one file, and restore them later or on
        another PC. Your photos themselves are not in it.
      </p>
    </div>
  </div>
  <div class="actions">
    <button class="btn" onclick={backUp} disabled={busy}><Icon name="download" size={20} /> Back up…</button>
    <button class="btn" onclick={restore} disabled={busy}><Icon name="restore" size={20} /> Restore…</button>
    {#if busy}<span class="spinner"></span>{/if}
  </div>
  {#if error}<p class="error">{error}</p>{/if}
  {#if restored}
    <p class="muted small">
      Restored {n(restored.items, 'item', 'items')}{restored.foldersAdded
        ? ` (${n(restored.foldersAdded, 'folder', 'folders')} added)`
        : ''}: {n(restored.favorites, 'new favorite', 'new favorites')}, {n(
        restored.albumsCreated,
        'album made',
        'albums made',
      )}, {n(restored.albumItems, 'item', 'items')} added to albums, {n(restored.peopleNamed, 'person', 'people')} named.
      {#if restored.facesWaiting}
        {n(restored.facesWaiting, 'face is', 'faces are')} named once recognition finds {restored.facesWaiting === 1
          ? 'it'
          : 'them'}.
      {/if}
      {#if restored.notFound}
        {n(restored.notFound, 'item was', 'items were')} not found (their folder isn't on this PC, or the files are gone).
      {/if}
    </p>
  {/if}
</div>

<style>
  .actions {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 4px;
  }
  .small {
    font-size: 13px;
  }
</style>
