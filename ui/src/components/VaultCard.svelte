<script lang="ts">
  import { ApiError, api, errorMessage, type Settings, type VaultState } from '../lib/api'
  import { promptDialog } from '../lib/dialogs.svelte'
  import Icon from '../lib/Icon.svelte'
  import { toast } from '../lib/toast.svelte'

  /** Locked folder options: auto-lock, password and recovery key. */
  let { settings, onchange }: { settings: Settings; onchange: (s: Settings) => void } = $props()

  let vault = $state<VaultState | null>(null)
  let error = $state('')
  let newKey = $state<string | null>(null)

  const MINUTES = [1, 5, 15, 30, 60]

  $effect(() => {
    api.vault
      .state()
      .then((s) => (vault = s))
      .catch(() => {})
  })

  async function save(patch: Partial<Settings>) {
    error = ''
    try {
      onchange(await api.settings.update(patch))
    } catch (e) {
      error = errorMessage(e)
    }
  }

  const wrong = (e: unknown) => e instanceof ApiError && e.status === 403

  async function changePassword() {
    error = ''
    const old = await promptDialog({ title: 'Change password', label: 'Current password', password: true, confirmLabel: 'Next' })
    if (old === null) return
    let problem: string | undefined
    for (;;) {
      const next = await promptDialog({
        title: 'Change password',
        label: 'New password (at least 6 characters)',
        password: true,
        error: problem,
        confirmLabel: 'Next',
      })
      if (next === null) return
      if (next.length < 6) {
        problem = 'Use at least 6 characters.'
        continue
      }
      const again = await promptDialog({ title: 'Change password', label: 'Type the new password again', password: true, confirmLabel: 'Change' })
      if (again === null) return
      if (again !== next) {
        problem = "The passwords didn't match. Try again."
        continue
      }
      try {
        await api.vault.changePassword(old, next)
        toast('Password changed')
      } catch (e) {
        error = wrong(e) ? 'The current password was wrong; nothing changed.' : errorMessage(e)
      }
      return
    }
  }

  async function makeRecoveryKey() {
    error = ''
    const password = await promptDialog({
      title: 'New recovery key',
      message: 'Your old recovery key stops working.',
      label: 'Password',
      password: true,
      confirmLabel: 'Make new key',
    })
    if (password === null) return
    try {
      newKey = (await api.vault.newRecoveryKey(password)).recoveryKey
    } catch (e) {
      error = wrong(e) ? 'Wrong password; the recovery key is unchanged.' : errorMessage(e)
    }
  }

  async function copyKey() {
    if (newKey) {
      await navigator.clipboard.writeText(newKey)
      toast('Recovery key copied')
    }
  }
</script>

<div class="card">
  <div class="card-head">
    <div>
      <h2>Locked folder</h2>
      <p class="muted">
        Encrypted photos and videos behind a password, kept in their photos folder's hidden <code>.memora</code> folder.
        Never shown on the TV or the network.
      </p>
    </div>
    <a class="btn" href="#/vault"><Icon name="lock" size={20} /> Open</a>
  </div>
  {#if vault?.exists}
    <label class="field-row">
      <span>Lock after</span>
      <select value={settings.vaultLockMinutes} onchange={(e) => save({ vaultLockMinutes: Number(e.currentTarget.value) })}>
        {#each MINUTES as m (m)}
          <option value={m}>{m === 1 ? '1 minute' : `${m} minutes`} without using it</option>
        {/each}
      </select>
    </label>
    <label class="check-line">
      <input
        type="checkbox"
        checked={settings.vaultLockOnMinimize}
        onchange={(e) => save({ vaultLockOnMinimize: e.currentTarget.checked })}
      />
      Also lock when Memora is minimized
    </label>
    <p class="muted small">It always locks when Windows locks or Memora closes.</p>
    {#if vault.unlocked}
      <div class="vault-actions">
        <button class="btn" onclick={changePassword}><Icon name="key" size={20} /> Change password…</button>
        <button class="btn" onclick={makeRecoveryKey}><Icon name="refresh" size={20} /> New recovery key…</button>
      </div>
    {:else}
      <p class="muted small">Unlock the locked folder to change its password or recovery key.</p>
    {/if}
    {#if newKey}
      <p>Your new recovery key. Save it now; Memora won't show it again.</p>
      <div class="recovery-key">{newKey}</div>
      <div class="vault-actions">
        <button class="btn" onclick={copyKey}><Icon name="contentCopy" size={20} /> Copy</button>
        <button class="btn" onclick={() => (newKey = null)}>Done</button>
      </div>
    {/if}
  {:else if vault}
    <p class="muted small">Not set up yet.</p>
  {/if}
  {#if error}<p class="error">{error}</p>{/if}
</div>

<style>
  .field-row {
    display: flex;
    align-items: center;
    gap: 12px;
    margin: 8px 0;
  }
  .field-row select {
    height: 36px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
  }
  .small {
    font-size: 13px;
  }
</style>
