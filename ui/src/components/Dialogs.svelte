<script lang="ts">
  import { dialogs } from '../lib/dialogs.svelte'

  let input = $state<HTMLInputElement>()

  const d = $derived(dialogs.current)

  $effect(() => {
    if (d?.kind === 'prompt') input?.focus()
  })

  function answer(ok: boolean) {
    const cur = dialogs.current
    if (!cur) return
    dialogs.current = null
    if (cur.kind === 'confirm') cur.resolve(ok)
    else cur.resolve(ok && cur.value.trim() ? cur.value.trim() : null)
  }

  function onkeydown(e: KeyboardEvent) {
    if (!d) return
    if (e.key === 'Escape') answer(false)
    else if (e.key === 'Enter') answer(true)
    else return
    e.preventDefault()
    e.stopImmediatePropagation() // also keeps an album picker underneath from handling the key
  }
</script>

<svelte:window onkeydowncapture={onkeydown} />

{#if d}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="modal-backdrop" onclick={(e) => e.target === e.currentTarget && answer(false)}>
    <div class="modal" role="dialog" aria-modal="true" aria-label={d.title}>
      <h2>{d.title}</h2>
      {#if d.kind === 'confirm'}
        <p>{d.message}</p>
      {:else}
        <label class="field">
          <span>{d.label}</span>
          <input type="text" bind:this={input} bind:value={d.value} maxlength="200" />
        </label>
      {/if}
      <div class="modal-actions">
        <button class="btn" onclick={() => answer(false)}>Cancel</button>
        <button
          class="btn primary"
          class:danger={d.kind === 'confirm' && d.danger}
          disabled={d.kind === 'prompt' && !d.value.trim()}
          onclick={() => answer(true)}>{d.confirmLabel}</button
        >
      </div>
    </div>
  </div>
{/if}
