<script lang="ts">
  // Minimize, maximize/restore and close, drawn by the app because the window has no Windows
  // title bar. Always on top of everything (viewer, dialogs), so the window can always be closed.
  import { invoke } from '@tauri-apps/api/core'
  import { listen } from '@tauri-apps/api/event'
  import { appWindow, closeWindow, minimizeWindow, showSystemMenu, toggleMaximizeWindow } from '../lib/window.svelte'

  // Segoe Fluent Icons glyphs.
  const MAXIMIZE = String.fromCharCode(0xe922)
  const RESTORE = String.fromCharCode(0xe923)

  let maxButton: HTMLButtonElement | undefined = $state()
  /** Hover/press of the maximize button. A native stand-in sits over it (for Windows 11 Snap
   *  Layouts) and gets the mouse instead of the page, so it reports them as events. */
  let maxState = $state<'hover' | 'press' | 'leave'>('leave')

  // Tell the app where the maximize button is, now and whenever the layout may have changed.
  $effect(() => {
    const el = maxButton
    if (!el) return
    const report = () => {
      const r = el.getBoundingClientRect()
      invoke('set_max_button_rect', {
        right: window.innerWidth - r.right,
        top: r.top,
        width: r.width,
        height: r.height,
      }).catch(() => {})
    }
    report()
    const observer = new ResizeObserver(report)
    observer.observe(el)
    window.addEventListener('resize', report)
    return () => {
      observer.disconnect()
      window.removeEventListener('resize', report)
    }
  })

  $effect(() => {
    const off = listen<'hover' | 'press' | 'leave'>('max-button', (e) => (maxState = e.payload))
    // The page only sees the pointer when it is not on the stand-in.
    const away = () => {
      if (maxState !== 'leave') maxState = 'leave'
    }
    window.addEventListener('pointermove', away)
    return () => {
      off.then((f) => f())
      window.removeEventListener('pointermove', away)
    }
  })

  function oncontextmenu(e: MouseEvent) {
    e.preventDefault()
    showSystemMenu(e.clientX, e.clientY)
  }
</script>

<!-- Like native caption buttons: not in the Tab order (Alt+Space and Alt+F4 work from the keyboard). -->
<div class="window-controls" class:inactive={!appWindow.focused} {oncontextmenu} role="group" aria-label="Window">
  <button class="wc" tabindex="-1" title="Minimize" aria-label="Minimize" onclick={minimizeWindow}>&#xE921;</button>
  <button
    bind:this={maxButton}
    class="wc"
    class:hover={maxState === 'hover'}
    class:press={maxState === 'press'}
    tabindex="-1"
    title={appWindow.maximized ? 'Restore down' : 'Maximize'}
    aria-label={appWindow.maximized ? 'Restore down' : 'Maximize'}
    onclick={toggleMaximizeWindow}
  >
    {appWindow.maximized ? RESTORE : MAXIMIZE}
  </button>
  <button class="wc close" tabindex="-1" title="Close" aria-label="Close" onclick={closeWindow}>&#xE8BB;</button>
</div>
