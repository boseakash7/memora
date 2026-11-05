<script lang="ts">
  import { monthLabel } from '../lib/dates'
  import type { MonthMark } from '../lib/layout'

  let {
    months,
    height,
    viewport,
    scrollTop,
    onseek,
  }: {
    months: MonthMark[]
    height: number
    viewport: number
    scrollTop: number
    onseek: (top: number) => void
  } = $props()

  let track = $state(0)
  let hoverY = $state<number | null>(null)
  let dragging = $state(false)

  const maxScroll = $derived(Math.max(1, height - viewport))
  const toY = (top: number) => (Math.min(1, top / maxScroll) * track) | 0
  const toTop = (y: number) => (Math.max(0, Math.min(track, y)) / Math.max(1, track)) * maxScroll

  /** Year labels, dropped where they would overlap the one above. */
  const years = $derived.by(() => {
    const out: { year: number; y: number }[] = []
    let last = -Infinity
    for (const m of months) {
      if (out.length && out[out.length - 1].year === m.year) continue
      const y = Math.max(8, toY(m.top)) // keep the first label inside the track
      if (y - last < 20) continue
      out.push({ year: m.year, y })
      last = y
    }
    return out
  })

  const dots = $derived.by(() => {
    const out: number[] = []
    let last = -Infinity
    for (const m of months) {
      const y = toY(m.top)
      if (y - last >= 6) {
        out.push(y)
        last = y
      }
    }
    return out
  })

  function monthAt(top: number): string {
    let found = months[0]
    for (const m of months) {
      if (m.top > top + viewport / 4) break
      found = m
    }
    return found ? monthLabel(found.year, found.month) : ''
  }

  function localY(e: PointerEvent) {
    return e.clientY - (e.currentTarget as HTMLElement).getBoundingClientRect().top
  }

  function down(e: PointerEvent) {
    dragging = true
    ;(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId)
    onseek(toTop(localY(e)))
  }

  function move(e: PointerEvent) {
    hoverY = Math.max(0, Math.min(track, localY(e)))
    if (dragging) onseek(toTop(hoverY))
  }

  function up() {
    dragging = false
  }
</script>

<div
  class="scrubber"
  class:active={dragging}
  bind:clientHeight={track}
  onpointerdown={down}
  onpointermove={move}
  onpointerup={up}
  onpointerleave={() => !dragging && (hoverY = null)}
  role="scrollbar"
  aria-controls="photos"
  aria-valuenow={Math.round((scrollTop / maxScroll) * 100)}
  tabindex="-1"
>
  {#each dots as y (y)}
    <span class="scrub-dot" style:top="{y}px"></span>
  {/each}
  {#each years as m (m.year)}
    <span class="scrub-year" style:top="{m.y}px">{m.year}</span>
  {/each}
  <span class="scrub-pos" style:top="{toY(scrollTop)}px"></span>
  {#if hoverY !== null}
    <span class="scrub-hover" style:top="{hoverY}px">
      <span class="scrub-bubble">{monthAt(toTop(hoverY))}</span>
    </span>
  {/if}
</div>
