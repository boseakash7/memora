<script lang="ts">
  import { open } from '@tauri-apps/plugin-dialog'
  import { onDestroy } from 'svelte'
  import PhotoGrid from '../components/PhotoGrid.svelte'
  import VaultViewer from '../components/VaultViewer.svelte'
  import {
    ApiError,
    api,
    errorMessage,
    vaultUrl,
    type TimelineItem,
    type VaultItem,
    type VaultState,
  } from '../lib/api'
  import { countLabel } from '../lib/actions'
  import { confirmDialog } from '../lib/dialogs.svelte'
  import Icon from '../lib/Icon.svelte'
  import { toast } from '../lib/toast.svelte'

  const MIN_PASSWORD = 6

  let vs = $state<VaultState | null>(null)
  let items = $state.raw<VaultItem[]>([])
  let error = $state('')
  let busy = $state(false)

  // Set up / unlock forms.
  let password = $state('')
  let password2 = $state('')
  let recoveryInput = $state('')
  let forgot = $state(false)
  /** Shown once after setup (or a new key), until the user says it is saved. */
  let recoveryKey = $state<string | null>(null)
  let saved = $state(false)

  // Grid.
  let openIndex = $state<number | null>(null)
  let selected = $state.raw<ReadonlySet<number>>(new Set())
  let lastToggled: number | null = null

  /** The grid wants numbers: an item's place in the list, plus one. */
  const gridItems = $derived<TimelineItem[]>(
    items.map((it, i) => ({
      id: i + 1,
      w: it.width ?? 0,
      h: it.height ?? 0,
      t: it.takenAt,
      v: it.kind === 'video' || undefined,
      d: it.durationMs ?? undefined,
      m: 0,
      f: it.favorite || undefined,
    })),
  )
  const selectedItems = $derived(items.filter((_, i) => selected.has(i + 1)))

  async function refresh() {
    try {
      const s = await api.vault.state()
      vs = s
      if (s.unlocked) items = await api.vault.items()
      else {
        items = []
        openIndex = null
        selected = new Set()
      }
      error = ''
    } catch (e) {
      if (e instanceof ApiError && e.status === 423) return refresh()
      error = errorMessage(e)
    }
  }
  refresh()

  // It locks itself after a while without use (and when Windows locks): notice it.
  const poll = setInterval(() => {
    if (vs?.unlocked) {
      api.vault
        .state()
        .then((s) => {
          if (!s.unlocked) refresh()
        })
        .catch(() => {})
    }
  }, 5000)
  onDestroy(() => clearInterval(poll))

  /** Run an action; a "locked" answer brings back the unlock screen. */
  async function attempt(action: () => Promise<void>) {
    busy = true
    error = ''
    try {
      await action()
    } catch (e) {
      if (e instanceof ApiError && e.status === 423) await refresh()
      else if (e instanceof ApiError && e.status === 403) error = 'Wrong password. Try again.'
      else error = errorMessage(e)
    } finally {
      busy = false
    }
  }

  const passwordProblem = $derived(
    password.length > 0 && password.length < MIN_PASSWORD
      ? `Use at least ${MIN_PASSWORD} characters.`
      : password2 && password !== password2
        ? "The passwords don't match."
        : '',
  )

  const setUp = () =>
    attempt(async () => {
      const r = await api.vault.setup(password)
      recoveryKey = r.recoveryKey
      saved = false
      password = password2 = ''
    })

  const unlock = () =>
    attempt(async () => {
      await api.vault.unlock(password)
      password = ''
      await refresh()
    })

  const recover = () =>
    attempt(async () => {
      await api.vault.recover(recoveryInput, password)
      password = password2 = recoveryInput = ''
      forgot = false
      toast('Unlocked with your recovery key. Your new password is set.')
      await refresh()
    })

  const lock = () =>
    attempt(async () => {
      await api.vault.lock()
      await refresh()
    })

  async function copyKey() {
    if (!recoveryKey) return
    await navigator.clipboard.writeText(recoveryKey)
    toast('Recovery key copied')
  }

  async function keySaved() {
    recoveryKey = null
    await refresh()
  }

  // ---------- selection and actions ----------

  function toggle(index: number, range: boolean) {
    const next = new Set(selected)
    const id = index + 1
    if (range && lastToggled !== null) {
      const on = !selected.has(id)
      const [a, b] = lastToggled < index ? [lastToggled, index] : [index, lastToggled]
      for (let i = a; i <= b; i++) {
        if (on) next.add(i + 1)
        else next.delete(i + 1)
      }
    } else if (next.has(id)) next.delete(id)
    else next.add(id)
    lastToggled = index
    selected = next
  }

  function toggleDay(start: number, end: number) {
    const ids = Array.from({ length: end - start }, (_, k) => start + k + 1)
    const next = new Set(selected)
    if (ids.every((id) => next.has(id))) ids.forEach((id) => next.delete(id))
    else ids.forEach((id) => next.add(id))
    lastToggled = start
    selected = next
  }

  function clearSelection() {
    selected = new Set()
    lastToggled = null
  }

  const favorite = (list: VaultItem[]) =>
    attempt(async () => {
      const next = !list.every((i) => i.favorite)
      await api.vault.setFavorite(
        list.map((i) => i.id),
        next,
      )
      items = items.map((i) => (list.some((l) => l.id === i.id) ? { ...i, favorite: next } : i))
    })

  const moveOut = (list: VaultItem[]) =>
    attempt(async () => {
      const dir = await open({ directory: true, multiple: false, title: 'Choose where to put the files' })
      if (typeof dir !== 'string') return
      const ok = await confirmDialog({
        title: list.length === 1 ? 'Move out of the locked folder?' : `Move ${countLabel(list.length)} out?`,
        message:
          `They are decrypted into ${dir} and are no longer protected. ` +
          'If that folder is in your library, they show up in Photos again.',
        confirmLabel: 'Move out',
      })
      if (!ok) return
      const r = await api.vault.moveOut(
        list.map((i) => i.id),
        dir,
      )
      clearSelection()
      openIndex = null
      await refresh()
      if (r.failed.length > 0) toast(`Not moved: ${r.failed[0]}`, { error: true })
      else toast(r.moved === 1 ? 'Moved out of the locked folder' : `Moved ${countLabel(r.moved)} out`)
    })

  const remove = (list: VaultItem[]) =>
    attempt(async () => {
      const ok = await confirmDialog({
        title: list.length === 1 ? 'Delete this item forever?' : `Delete ${countLabel(list.length)} forever?`,
        message: "They are deleted from the locked folder right away. There is no trash here, and it can't be undone.",
        confirmLabel: 'Delete forever',
        danger: true,
      })
      if (!ok) return
      await api.vault.delete(list.map((i) => i.id))
      clearSelection()
      openIndex = null
      await refresh()
      toast(list.length === 1 ? 'Deleted' : `Deleted ${countLabel(list.length)}`)
    })

  function onkeydown(e: KeyboardEvent) {
    if (openIndex !== null || !vs?.unlocked) return
    const target = e.target as HTMLElement | null
    if (target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA')) return
    if (e.key === 'Escape' && selected.size > 0) clearSelection()
    else if ((e.ctrlKey || e.metaKey) && e.key === 'a' && items.length > 0) selected = new Set(gridItems.map((i) => i.id))
    else return
    e.preventDefault()
  }
</script>

<svelte:window {onkeydown} />

{#if recoveryKey}
  <section class="page vault-intro">
    <div class="vault-hero"><Icon name="key" size={48} /></div>
    <h1>Save your recovery key</h1>
    <p>
      If you forget your password, this key is the only way to open your locked folder. Write it down or keep it in a
      password manager. Memora shows it only now.
    </p>
    <div class="recovery-key" aria-label="Recovery key">{recoveryKey}</div>
    <div class="vault-actions">
      <button class="btn" onclick={copyKey}><Icon name="contentCopy" size={20} /> Copy</button>
    </div>
    <label class="check-line">
      <input type="checkbox" bind:checked={saved} />
      I've saved my recovery key somewhere safe
    </label>
    <div class="vault-actions">
      <button class="btn primary" disabled={!saved} onclick={keySaved}>Continue</button>
    </div>
  </section>
{:else if vs === null}
  {#if error}<section class="page"><p class="error">{error}</p></section>{/if}
{:else if !vs.exists}
  <section class="page vault-intro">
    <div class="vault-hero"><Icon name="lock" size={48} /></div>
    <h1>Locked folder</h1>
    <p>
      Keep private photos and videos encrypted. They open only in Memora with your password, and never show
      up in Photos, search, Memories, on the TV or on the network.
    </p>
    <ul class="vault-points muted">
      <li>Moving an item in encrypts it and deletes the original file from its folder.</li>
      <li>If you forget the password, you need the recovery key you get next. Without both, the items are lost.</li>
      <li>
        Encrypted items stay in their photos folder (in its hidden <code>.memora</code> folder), so they move and back up
        with it. Another PC that adds the folder opens them with the same password.
      </li>
    </ul>
    <form
      class="vault-form"
      onsubmit={(e) => {
        e.preventDefault()
        setUp()
      }}
    >
      <label class="field">
        <span>Password</span>
        <input type="password" bind:value={password} autocomplete="new-password" />
      </label>
      <label class="field">
        <span>Type it again</span>
        <input type="password" bind:value={password2} autocomplete="new-password" />
      </label>
      {#if passwordProblem}<p class="muted">{passwordProblem}</p>{/if}
      {#if error}<p class="error">{error}</p>{/if}
      <div class="vault-actions">
        <button
          class="btn primary"
          type="submit"
          disabled={busy || password.length < MIN_PASSWORD || password !== password2}>Set up locked folder</button
        >
      </div>
    </form>
  </section>
{:else if !vs.unlocked}
  <section class="page vault-intro">
    <div class="vault-hero"><Icon name="lock" size={48} /></div>
    <h1>Locked folder</h1>
    {#if !forgot}
      <form
        class="vault-form"
        onsubmit={(e) => {
          e.preventDefault()
          unlock()
        }}
      >
        <label class="field">
          <span>Password</span>
          <!-- svelte-ignore a11y_autofocus -->
          <input type="password" bind:value={password} autocomplete="current-password" autofocus />
        </label>
        {#if error}<p class="error">{error}</p>{/if}
        <div class="vault-actions">
          <button class="btn primary" type="submit" disabled={busy || !password}>
            <Icon name="lockOpen" size={20} /> Unlock
          </button>
          <button class="btn" type="button" onclick={() => ((forgot = true), (error = ''), (password = ''))}>
            Forgot password?
          </button>
        </div>
      </form>
    {:else}
      <p>Enter your recovery key and choose a new password.</p>
      <form
        class="vault-form"
        onsubmit={(e) => {
          e.preventDefault()
          recover()
        }}
      >
        <label class="field">
          <span>Recovery key</span>
          <input class="mono" bind:value={recoveryInput} placeholder="XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX" />
        </label>
        <label class="field">
          <span>New password</span>
          <input type="password" bind:value={password} autocomplete="new-password" />
        </label>
        <label class="field">
          <span>Type it again</span>
          <input type="password" bind:value={password2} autocomplete="new-password" />
        </label>
        {#if passwordProblem}<p class="muted">{passwordProblem}</p>{/if}
        {#if error}<p class="error">{error === 'Wrong password. Try again.' ? "That recovery key doesn't fit." : error}</p>{/if}
        <div class="vault-actions">
          <button
            class="btn primary"
            type="submit"
            disabled={busy || !recoveryInput.trim() || password.length < MIN_PASSWORD || password !== password2}
            >Unlock and set password</button
          >
          <button class="btn" type="button" onclick={() => ((forgot = false), (error = ''))}>Back</button>
        </div>
      </form>
    {/if}
  </section>
{:else}
  <div class="media-page">
    <div class="page-bar">
      <div class="page-bar-title">
        <h1>Locked folder</h1>
        <span class="muted">{countLabel(items.length)} · encrypted</span>
      </div>
      <div class="page-bar-actions">
        <a class="btn" href="#/settings" title="Auto-lock, password and recovery key">
          <Icon name="settings" size={20} /> Options
        </a>
        <button class="btn" onclick={lock} disabled={busy}><Icon name="lock" size={20} /> Lock</button>
      </div>
    </div>
    {#if error}<p class="error">{error}</p>{/if}
    <div class="grid-host">
      {#if items.length === 0}
        <section class="page">
          <div class="empty">
            <div class="empty-icon"><Icon name="lock" size={64} /></div>
            <h2>Nothing here yet</h2>
            <p>
              Select photos or videos anywhere in Memora, then choose Move to locked folder (the lock button, or
              right-click).
            </p>
          </div>
        </section>
      {:else}
        <PhotoGrid
          items={gridItems}
          {selected}
          thumbUrl={(it) => vaultUrl(items[it.id - 1].id, 'thumb')}
          onopen={(i) => (openIndex = i)}
          ontoggle={toggle}
          ontoggleday={toggleDay}
        />
      {/if}
    </div>

    {#if selected.size > 0}
      <div class="selection-bar">
        <button class="icon-btn" title="Clear selection (Esc)" onclick={clearSelection}><Icon name="close" /></button>
        <span class="selection-count">{selected.size.toLocaleString()} selected</span>
        <span class="viewer-spacer"></span>
        <button class="icon-btn" title="Favorite" onclick={() => favorite(selectedItems)}>
          <Icon name={selectedItems.every((i) => i.favorite) ? 'star' : 'starOutline'} />
        </button>
        <button class="btn" onclick={() => moveOut(selectedItems)}><Icon name="moveOut" size={20} /> Move out…</button>
        <button class="btn danger" onclick={() => remove(selectedItems)}>
          <Icon name="deleteForever" size={20} /> Delete
        </button>
      </div>
    {/if}

    {#if openIndex !== null && items[openIndex]}
      <VaultViewer
        {items}
        index={openIndex}
        onnavigate={(i) => (openIndex = i)}
        onclose={() => (openIndex = null)}
        onfavorite={(it) => favorite([it])}
        onmoveout={(it) => moveOut([it])}
        ondelete={(it) => remove([it])}
      />
    {/if}
  </div>
{/if}
