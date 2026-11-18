<script lang="ts">
  import { flip } from 'svelte/animate'
  import { dismiss, toasts } from '../lib/toast.svelte'
  import { motion, pop } from '../lib/motion.svelte'

  const flipMs = $derived(motion.level === 'high' ? 320 : motion.level === 'medium' ? 180 : 0)
</script>

<div class="toasts" aria-live="polite">
  {#each toasts.list as t (t.id)}
    <div
      class="toast"
      class:error={t.error}
      animate:flip={{ duration: flipMs }}
      in:pop={{ y: 24, scale: 0.9, duration: 360 }}
      out:pop={{ y: 12, scale: 0.96, duration: 200 }}
    >
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
