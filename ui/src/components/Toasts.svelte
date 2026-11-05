<script lang="ts">
  import { dismiss, toasts } from '../lib/toast.svelte'
</script>

<div class="toasts" aria-live="polite">
  {#each toasts.list as t (t.id)}
    <div class="toast" class:error={t.error}>
      <span>{t.message}</span>
      {#if t.action}
        {@const action = t.action}
        <button
          class="toast-action"
          onclick={() => {
            dismiss(t.id)
            action.run()
          }}>{action.label}</button
        >
      {/if}
    </div>
  {/each}
</div>
