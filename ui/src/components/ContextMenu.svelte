<script lang="ts">
  import { tick } from 'svelte'
  import { closeContextMenu, contextMenu } from '../lib/contextMenu.svelte'
  import Icon from '../lib/Icon.svelte'

  let el = $state<HTMLDivElement>()
  let left = $state(0)
  let top = $state(0)
  /** Where focus was before the menu opened, to give it back on close. */
  let returnFocus: HTMLElement | null = null

  const menu = $derived(contextMenu.current)

  // Place the menu at the pointer, flipped or nudged to stay inside the window.
  $effect(() => {
    const m = menu
    if (!m) return
    returnFocus = document.activeElement as HTMLElement | null
    left = m.x
    top = m.y
    tick().then(() => {
      if (!el || contextMenu.current !== m) return
      const { width, height } = el.getBoundingClientRect()
      const margin = 4
      if (m.x + width > innerWidth - margin) left = Math.max(margin, m.x - width)
      if (m.y + height > innerHeight - margin) top = Math.max(margin, innerHeight - margin - height)
      if (m.keyboard) items()[0]?.focus()
      else el.focus()
    })
  })

  // Close on anything that moves the page under it.
  $effect(() => {
    if (!menu) return
    const close = () => closeContextMenu()
    const outside = (e: PointerEvent) => {
      if (el && !el.contains(e.target as Node)) close()
    }
    window.addEventListener('pointerdown', outside, true)
    window.addEventListener('scroll', close, true)
    window.addEventListener('wheel', close, true)
    window.addEventListener('resize', close)
    window.addEventListener('blur', close)
    window.addEventListener('hashchange', close)
    return () => {
      window.removeEventListener('pointerdown', outside, true)
      window.removeEventListener('scroll', close, true)
      window.removeEventListener('wheel', close, true)
      window.removeEventListener('resize', close)
      window.removeEventListener('blur', close)
      window.removeEventListener('hashchange', close)
    }
  })

  function items(): HTMLButtonElement[] {
    return el ? [...el.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')] : []
  }

  function close(restoreFocus: boolean) {
    closeContextMenu()
    if (restoreFocus) returnFocus?.focus?.()
  }

  function onkeydown(e: KeyboardEvent) {
    const list = items()
    const at = list.indexOf(document.activeElement as HTMLButtonElement)
    if (e.key === 'ArrowDown') list[(at + 1) % list.length]?.focus()
    else if (e.key === 'ArrowUp') list[(at - 1 + list.length) % list.length]?.focus()
    else if (e.key === 'Home') list[0]?.focus()
    else if (e.key === 'End') list[list.length - 1]?.focus()
    else if (e.key === 'Escape' || e.key === 'Tab') close(true)
    else return
    e.preventDefault()
    e.stopPropagation()
  }

  function choose(run: () => void) {
    close(true)
    run()
  }
</script>

{#if menu}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    class="context-menu"
    role="menu"
    tabindex="-1"
    bind:this={el}
    style:left="{left}px"
    style:top="{top}px"
    {onkeydown}
    oncontextmenu={(e) => e.preventDefault()}
  >
    {#if menu.header}<div class="context-menu-header">{menu.header}</div>{/if}
    {#each menu.entries as entry, i (i)}
      {#if entry === 'separator'}
        <div class="context-menu-separator" role="separator"></div>
      {:else}
        <button
          class="context-menu-item"
          class:danger={entry.danger}
          role="menuitem"
          tabindex="-1"
          disabled={entry.disabled}
          onclick={() => choose(entry.run)}
        >
          <span class="context-menu-icon">{#if entry.icon}<Icon name={entry.icon} size={20} />{/if}</span>
          <span class="context-menu-label">{entry.label}</span>
          {#if entry.shortcut}<span class="context-menu-shortcut">{entry.shortcut}</span>{/if}
        </button>
      {/if}
    {/each}
  </div>
{/if}
