<script lang="ts">
  import { untrack } from 'svelte'
  import { mediaUrl, type TimelineItem } from '../lib/api'
  import { dayLabel, formatDuration } from '../lib/dates'
  import Icon from '../lib/Icon.svelte'
  import { firstBlockAfter, justify, type Layout } from '../lib/layout'
  import Scrubber from './Scrubber.svelte'

  let {
    items,
    selected,
    onopen,
    ontoggle,
    ontoggleday,
    onmenu,
  }: {
    items: TimelineItem[]
    /** Ids of the selected items. */
    selected: ReadonlySet<number>
    onopen: (index: number) => void
    /** Select or deselect one item; with `range`, everything from the last toggled item to this one. */
    ontoggle: (index: number, range: boolean) => void
    /** Select a whole day (or deselect it, if all of it is selected). `[start, end)` indices. */
    ontoggleday: (start: number, end: number) => void
    /** Right-click (or Shift+F10 / Menu key) on an item. */
    onmenu?: (index: number, e: MouseEvent) => void
  } = $props()

  const selecting = $derived(selected.size > 0)

  /** First and one-past-last item index of each day. */
  const dayRanges = $derived.by(() => {
    const ranges = new Map<string, [number, number]>()
    for (let i = 0; i < items.length; i++) {
      const day = items[i].t.slice(0, 10)
      const r = ranges.get(day)
      if (r) r[1] = i + 1
      else ranges.set(day, [i, i + 1])
    }
    return ranges
  })

  function dayFullySelected(day: string): boolean {
    const r = dayRanges.get(day)
    if (!r) return false
    for (let i = r[0]; i < r[1]; i++) if (!selected.has(items[i].id)) return false
    return true
  }

  function clickCell(e: MouseEvent, index: number) {
    if (selecting || e.shiftKey || e.ctrlKey || e.metaKey) ontoggle(index, e.shiftKey)
    else onopen(index)
  }

  /** Extra pixels rendered above and below the viewport, so fast scrolling doesn't show gaps. */
  const OVERSCAN = 800
  /** Horizontal padding of .grid-scroller (left + right, the right side leaves room for the scrubber). */
  const GRID_PADDING = 16 + 48

  let scroller: HTMLDivElement
  let width = $state(0)
  let viewport = $state(0)
  let scrollTop = $state(0)
  let failed = $state(new Set<number>())

  const layout: Layout = $derived(justify(items, width))
  const visible = $derived.by(() => {
    const blocks = layout.blocks
    const start = firstBlockAfter(blocks, scrollTop - OVERSCAN)
    const out = []
    for (let i = start; i < blocks.length && blocks[i].top < scrollTop + viewport + OVERSCAN; i++) {
      out.push(blocks[i])
    }
    return out
  })

  // Keep the photo at the top of the screen in place when the layout changes
  // (window resized, or new photos indexed above it).
  let anchor: { id: number; offset: number } | null = null

  function rememberAnchor() {
    if (scrollTop < 40) {
      anchor = null // at the top: let new photos push in
      return
    }
    const blocks = layout.blocks
    for (let i = firstBlockAfter(blocks, scrollTop); i < blocks.length; i++) {
      const b = blocks[i]
      if (b.kind === 'row') {
        anchor = { id: items[b.cells[0].index].id, offset: scrollTop - b.top }
        return
      }
    }
  }

  $effect(() => {
    const l = layout
    untrack(() => {
      if (!anchor || !scroller) return
      const index = items.findIndex((it) => it.id === anchor!.id)
      if (index < 0) return
      const block = l.blocks[l.rowOfItem[index]]
      const target = Math.max(0, block.top + anchor.offset)
      if (Math.abs(target - scroller.scrollTop) > 1) {
        scroller.scrollTop = target
        scrollTop = target
      }
    })
  })

  function onscroll() {
    scrollTop = scroller.scrollTop
    rememberAnchor()
  }

  function seek(top: number) {
    scroller.scrollTop = top
  }

  /** Scroll so an item is visible (used when closing the viewer). */
  export function reveal(index: number) {
    const block = layout.blocks[layout.rowOfItem[index]]
    if (!block) return
    if (block.top < scrollTop || block.top + block.height > scrollTop + viewport) {
      scroller.scrollTop = Math.max(0, block.top - viewport / 3)
    }
  }

  $effect(() => {
    const ro = new ResizeObserver(() => {
      width = scroller.clientWidth - GRID_PADDING
      viewport = scroller.clientHeight
    })
    ro.observe(scroller)
    return () => ro.disconnect()
  })

  function markFailed(id: number) {
    failed = new Set(failed).add(id)
  }
</script>

<div class="grid-wrap" class:selecting>
  <div class="grid-scroller" bind:this={scroller} {onscroll}>
    <div class="grid-canvas" style:height="{layout.height}px">
      {#each visible as block (block.key)}
        {#if block.kind === 'header'}
          {@const range = dayRanges.get(block.day)}
          <h2
            class="day-header"
            style:top="{block.top}px"
            style:left="{block.left}px"
            style:width="{block.width}px"
            style:height="{block.height}px"
          >
            <span>{dayLabel(block.day)}</span>
            {#if range}
              {@const all = dayFullySelected(block.day)}
              <button
                class="day-check"
                class:on={all}
                title={all ? 'Deselect day' : 'Select day'}
                onclick={() => ontoggleday(range[0], range[1])}
              >
                <Icon name={all ? 'checkCircle' : 'circleOutline'} size={22} />
              </button>
            {/if}
          </h2>
        {:else}
          {#each block.cells as cell (cell.index)}
            {@const item = items[cell.index]}
            {@const isSelected = selected.has(item.id)}
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div
              class="cell"
              class:selected={isSelected}
              oncontextmenu={(e) => onmenu?.(cell.index, e)}
              style:top="{block.top}px"
              style:left="{cell.left}px"
              style:width="{cell.width}px"
              style:height="{block.height}px"
            >
              <button class="cell-open" onclick={(e) => clickCell(e, cell.index)} title={item.t.replace('T', ' ')}>
                {#if failed.has(item.id)}
                  <span class="cell-broken"><Icon name="brokenImage" size={32} /></span>
                {:else}
                  <img
                    src={mediaUrl(item.id, 'thumb', item.m)}
                    alt=""
                    draggable="false"
                    decoding="async"
                    onerror={() => markFailed(item.id)}
                  />
                {/if}
              </button>
              <button
                class="cell-check"
                class:on={isSelected}
                tabindex="-1"
                title={isSelected ? 'Deselect' : 'Select'}
                onclick={(e) => ontoggle(cell.index, e.shiftKey)}
              >
                <Icon name={isSelected ? 'checkCircle' : 'circleOutline'} size={24} />
              </button>
              {#if item.v}
                <span class="duration">
                  {item.d ? formatDuration(item.d) : ''}
                  <Icon name="play" size={16} />
                </span>
              {/if}
              {#if item.f}
                <span class="cell-star"><Icon name="star" size={18} /></span>
              {/if}
            </div>
          {/each}
        {/if}
      {/each}
    </div>
  </div>
  {#if layout.height > viewport * 2}
    <Scrubber months={layout.months} height={layout.height} {viewport} {scrollTop} onseek={seek} />
  {/if}
</div>
