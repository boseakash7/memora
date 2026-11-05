import type { TimelineItem } from './api'

/** A photo placed in a row. `index` points into the items array. */
export type Cell = { index: number; left: number; width: number }

export type Block =
  | { kind: 'header'; key: string; top: number; height: number; left: number; width: number; day: string }
  | { kind: 'row'; key: string; top: number; height: number; cells: Cell[] }

/** Where each month starts, for the date scrubber. */
export type MonthMark = { key: string; top: number; year: number; month: number }

export type Layout = {
  blocks: Block[]
  height: number
  months: MonthMark[]
  /** Block index of each item's row, to scroll an item into view. */
  rowOfItem: Int32Array
}

export type LayoutOptions = {
  rowHeight: number
  gap: number
  headerHeight: number
  sectionGap: number
  paddingTop: number
}

export const defaultLayout: LayoutOptions = {
  rowHeight: 220,
  gap: 4,
  headerHeight: 52,
  sectionGap: 12,
  paddingTop: 4,
}

function aspect(item: TimelineItem): number {
  if (!item.w || !item.h) return 1
  // Clamp extreme panoramas and slivers so one item can't take over a row.
  return Math.min(4, Math.max(0.3, item.w / item.h))
}

/** Space between days that share a line, and the narrowest slot a day gets (room for its heading). */
const DAY_GAP = 20
const MIN_DAY_WIDTH = 150

/**
 * Google Photos style "justified" layout. Items are grouped by day.
 * - A day too big for one row fills full-width rows, each scaled to fit the width at
 *   roughly `rowHeight`; its last row keeps the target height and stays left-aligned.
 * - Days that fit in one row share a line with their neighbours, so sparse days
 *   don't leave most of the screen empty.
 */
export function justify(items: TimelineItem[], width: number, opts: LayoutOptions = defaultLayout): Layout {
  const { rowHeight, gap, headerHeight, sectionGap, paddingTop } = opts
  const blocks: Block[] = []
  const months: MonthMark[] = []
  const rowOfItem = new Int32Array(items.length)
  let y = paddingTop
  width = Math.max(width, 100)

  let month = ''
  const markMonth = (i: number) => {
    const t = items[i].t
    const m = t.slice(0, 7)
    if (m !== month) {
      month = m
      months.push({ key: m, top: y, year: +t.slice(0, 4), month: +t.slice(5, 7) })
    }
  }

  // ---- a line of small days ----
  let lineHeaders: Block[] = []
  let lineCells: Cell[] = []
  let lineX = 0

  const flushLine = () => {
    if (lineHeaders.length === 0) return
    const rowIndex = blocks.length + lineHeaders.length
    blocks.push(...lineHeaders)
    for (const c of lineCells) rowOfItem[c.index] = rowIndex
    blocks.push({ kind: 'row', key: `r${lineCells[0].index}`, top: y + headerHeight, height: rowHeight, cells: lineCells })
    y += headerHeight + rowHeight + sectionGap
    lineHeaders = []
    lineCells = []
    lineX = 0
  }

  const placeSmallDay = (start: number, end: number, natural: number) => {
    const slot = Math.max(natural, MIN_DAY_WIDTH)
    if (lineHeaders.length > 0 && lineX + DAY_GAP + slot > width) flushLine()
    const x0 = lineHeaders.length > 0 ? lineX + DAY_GAP : 0
    markMonth(start)
    lineHeaders.push({ kind: 'header', key: `h${items[start].t.slice(0, 10)}`, top: y, height: headerHeight, left: x0, width: slot, day: items[start].t.slice(0, 10) })
    let x = x0
    for (let i = start; i < end; i++) {
      const w = aspect(items[i]) * rowHeight
      const left = Math.round(x)
      lineCells.push({ index: i, left, width: Math.max(1, Math.round(x + w) - left) })
      x += w + gap
    }
    lineX = x0 + slot
  }

  // ---- a day that needs full-width rows ----
  const placeBigDay = (start: number, end: number) => {
    flushLine()
    markMonth(start)
    const day = items[start].t.slice(0, 10)
    blocks.push({ kind: 'header', key: `h${day}`, top: y, height: headerHeight, left: 0, width, day })
    y += headerHeight

    let row: number[] = []
    let arSum = 0
    const emit = (full: boolean) => {
      const gaps = gap * (row.length - 1)
      const h = full ? (width - gaps) / arSum : Math.min(rowHeight, (width - gaps) / arSum)
      const cells: Cell[] = []
      let x = 0
      row.forEach((index, k) => {
        const w = aspect(items[index]) * h
        const left = Math.round(x)
        const right = full && k === row.length - 1 ? width : Math.round(x + w)
        cells.push({ index, left, width: Math.max(1, right - left) })
        rowOfItem[index] = blocks.length
        x += w + gap
      })
      const height = Math.round(h)
      blocks.push({ kind: 'row', key: `r${row[0]}`, top: y, height, cells })
      y += height + gap
      row = []
      arSum = 0
    }
    for (let i = start; i < end; i++) {
      row.push(i)
      arSum += aspect(items[i])
      if (arSum * rowHeight + gap * (row.length - 1) >= width) emit(true)
    }
    if (row.length) emit(false)
    y += sectionGap - gap
  }

  for (let start = 0; start < items.length; ) {
    const day = items[start].t.slice(0, 10)
    let end = start
    let natural = -gap
    while (end < items.length && items[end].t.startsWith(day)) {
      natural += aspect(items[end]) * rowHeight + gap
      end++
    }
    if (natural <= width) placeSmallDay(start, end, natural)
    else placeBigDay(start, end)
    start = end
  }
  flushLine()

  return { blocks, height: Math.ceil(y + paddingTop), months, rowOfItem }
}

/** Index of the first block that ends below `y` (binary search; blocks are sorted by top). */
export function firstBlockAfter(blocks: Block[], y: number): number {
  let lo = 0
  let hi = blocks.length
  while (lo < hi) {
    const mid = (lo + hi) >> 1
    if (blocks[mid].top + blocks[mid].height < y) lo = mid + 1
    else hi = mid
  }
  return lo
}
