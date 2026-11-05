<script lang="ts">
  import MediaPage from '../components/MediaPage.svelte'
  import { api, errorMessage } from '../lib/api'
  import { confirmDialog } from '../lib/dialogs.svelte'
  import Icon from '../lib/Icon.svelte'
  import { toast } from '../lib/toast.svelte'

  async function emptyTrash(count: number) {
    const ok = await confirmDialog({
      title: 'Empty trash?',
      message: `${count.toLocaleString()} ${count === 1 ? 'file is' : 'files are'} moved to the Windows Recycle Bin and removed from the library. You can still get them back from the Recycle Bin.`,
      confirmLabel: 'Empty trash',
      danger: true,
    })
    if (!ok) return
    try {
      const result = await api.items.emptyTrash()
      if (result.failed.length > 0) {
        const more = result.failed.length > 1 ? ` (and ${result.failed.length - 1} more)` : ''
        toast(`Could not delete ${result.failed[0]}${more}`, { error: true })
      } else {
        toast(`Moved ${result.deleted.toLocaleString()} ${result.deleted === 1 ? 'file' : 'files'} to the Recycle Bin`)
      }
    } catch (e) {
      toast(errorMessage(e), { error: true })
    }
  }
</script>

<MediaPage
  query={{ scope: 'trash' }}
  title="Trash"
  subtitle="Items are moved to the Recycle Bin after 30 days. Until then, they stay on disk."
>
  {#snippet actions(count)}
    <button class="btn" disabled={count === 0} onclick={() => emptyTrash(count)}>
      <Icon name="deleteForever" size={20} /> Empty trash
    </button>
  {/snippet}
  {#snippet empty()}
    <div class="empty">
      <div class="empty-icon"><Icon name="trash" size={56} /></div>
      <h2>Trash is empty</h2>
      <p>Items you move to the trash show up here for 30 days.</p>
    </div>
  {/snippet}
</MediaPage>
