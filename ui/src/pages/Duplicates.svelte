<script lang="ts">
  import { untrack } from 'svelte'
  import Viewer from '../components/Viewer.svelte'
  import { api, errorMessage, mediaUrl, type DupGroup, type DupItem, type TimelineItem } from '../lib/api'
  import { formatBytes, formatDuration } from '../lib/dates'
  import { confirmDialog } from '../lib/dialogs.svelte'
  import Icon from '../lib/Icon.svelte'
  import { isBusy, library } from '../lib/library.svelte'
  import { toast } from '../lib/toast.svelte'

  let groups = $state<DupGroup[] | null>(null)
  let error = $state('')
  /** Copy the user chose to keep instead of the suggestion, by group key. */
  let chosen = $state<Record<string, number>>({})
  let viewing = $state<{ items: TimelineItem[]; index: number } | null>(null)

  async function load() {
    try {
      groups = await api.duplicates()
      error = ''
    } catch (e) {
      error = errorMessage(e)
    }
  }

  $effect(() => {
    load()
  })

  // Reload after trashing, and as hashing of new photos finishes.
  let seen: number | null = null
  let timer: ReturnType<typeof setTimeout> | undefined
  $effect(() => {
    const revision = library.status?.revision ?? null
    untrack(() => {
      if (seen !== null && revision !== seen) {
        clearTimeout(timer)
        timer = setTimeout(load, 400)
      }
      seen = revision
    })
  })
  $effect(() => () => clearTimeout(timer))

  /** A group is identified by its smallest id, which stays put when others are trashed. */
  const keyOf = (g: DupGroup) => String(Math.min(...g.items.map((i) => i.id)))

  function keeperOf(g: DupGroup): number {
    const picked = chosen[keyOf(g)]
    return picked !== undefined && g.items.some((i) => i.id === picked) ? picked : g.items[0].id
  }

  const others = (g: DupGroup) => g.items.filter((i) => i.id !== keeperOf(g))

  const totals = $derived.by(() => {
    let files = 0
    let bytes = 0
    for (const g of groups ?? []) {
      for (const i of others(g)) {
        files++
        bytes += i.size
      }
    }
    return { files, bytes }
  })

  async function trashIds(ids: number[]) {
    try {
      await api.items.trash(ids)
      toast(`Moved ${ids.length} ${ids.length === 1 ? 'item' : 'items'} to trash`, {
        action: { label: 'Undo', run: () => api.items.restore(ids).catch((e) => toast(errorMessage(e), { error: true })) },
      })
    } catch (e) {
      toast(errorMessage(e), { error: true })
    }
  }

  const trashOthers = (g: DupGroup) => trashIds(others(g).map((i) => i.id))

  async function trashAll() {
    if (!groups) return
    const ok = await confirmDialog({
      title: 'Trash all duplicates?',
      message: `${totals.files.toLocaleString()} files (${formatBytes(totals.bytes)}) go to the trash, keeping one copy from each group. You can restore them from the trash for 30 days.`,
      confirmLabel: 'Move to trash',
      danger: true,
    })
    if (ok) await trashIds(groups.flatMap((g) => others(g).map((i) => i.id)))
  }

  function open(g: DupGroup, item: DupItem) {
    const items: TimelineItem[] = g.items.map((i) => ({
      id: i.id,
      w: i.w,
      h: i.h,
      t: i.takenAt,
      v: i.video || undefined,
      d: i.durationMs ?? undefined,
      m: i.m,
    }))
    viewing = { items, index: g.items.indexOf(item) }
  }

  const folderOf = (path: string) => path.replace(/[\\/][^\\/]*$/, '')

  const busy = $derived(isBusy(library.status))
</script>

<section class="page">
  <div class="page-head">
    <h1>Duplicates</h1>
    {#if groups && groups.length > 0}
      <button class="btn" onclick={trashAll}>
        <Icon name="trash" size={20} /> Trash all duplicates
      </button>
    {/if}
  </div>

  {#if error}
    <p class="error">{error}</p>
  {:else if groups === null}
    <!-- loading -->
  {:else if groups.length === 0}
    <div class="empty">
      <div class="empty-icon"><Icon name="utilities" size={56} /></div>
      {#if busy}
        <h2>Still checking…</h2>
        <p>Photos are compared once indexing finishes. Come back in a moment.</p>
      {:else}
        <h2>No duplicates found</h2>
        <p>Identical files and photos that look the same would show up here.</p>
      {/if}
    </div>
  {:else}
    <p class="muted">
      {groups.length.toLocaleString()}
      {groups.length === 1 ? 'group' : 'groups'} · removing the extra copies frees {formatBytes(totals.bytes)}.
      {#if busy}Still indexing, so more may appear.{/if}
    </p>

    {#each groups as group (keyOf(group))}
      {@const keeper = keeperOf(group)}
      <div class="card dup-group">
        <div class="card-head">
          <div>
            <strong>{group.kind === 'exact' ? 'Identical files' : 'Look the same'}</strong>
            <span class="muted"> · {group.items.length} copies · {formatBytes(others(group).reduce((n, i) => n + i.size, 0))} to free</span>
          </div>
          <button class="btn" onclick={() => trashOthers(group)}>
            <Icon name="trash" size={20} /> Trash {others(group).length === 1 ? 'the other' : `the other ${others(group).length}`}
          </button>
        </div>
        <div class="dup-items">
          {#each group.items as item (item.id)}
            <div class="dup-item" class:keep={item.id === keeper}>
              <button class="dup-thumb" onclick={() => open(group, item)} title="View">
                <img src={mediaUrl(item.id, 'thumb', item.m)} alt="" draggable="false" loading="lazy" />
                {#if item.video}
                  <span class="duration">{item.durationMs ? formatDuration(item.durationMs) : ''}<Icon name="play" size={16} /></span>
                {/if}
              </button>
              <div class="dup-meta">
                {#if item.id === keeper}
                  <span class="badge keep-badge">Keeping{item.id === group.items[0].id && chosen[keyOf(group)] === undefined ? ' (suggested)' : ''}</span>
                {:else}
                  <button class="link" onclick={() => (chosen[keyOf(group)] = item.id)}>Keep this one instead</button>
                {/if}
                <span class="dup-name" title={item.path}>{item.name}</span>
                <span class="muted">
                  {item.w && item.h ? `${item.w} × ${item.h} · ` : ''}{formatBytes(item.size)}
                </span>
                <span class="muted dup-folder" title={item.path}>{folderOf(item.path)}</span>
              </div>
            </div>
          {/each}
        </div>
      </div>
    {/each}
  {/if}
</section>

{#if viewing}
  <Viewer
    items={viewing.items}
    index={viewing.index}
    onnavigate={(i) => viewing && (viewing = { ...viewing, index: i })}
    onclose={() => (viewing = null)}
  />
{/if}
