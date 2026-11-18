import type { MemoryCard, MemoryKind } from './api'
import { parseLocal } from './dates'

const day = new Intl.DateTimeFormat(undefined, { day: 'numeric', month: 'short', year: 'numeric' })

function dateOf(s: string): Date {
  return parseLocal(`${s}T12:00:00`)
}

/** "18 Nov 2023", "2–6 Oct 2023", "28 Sep – 3 Oct 2023" (in the PC's language). */
export function dayRange(start: string, end: string): string {
  if (start === end) return day.format(dateOf(start))
  return day.formatRange(dateOf(start), dateOf(end))
}

/** The line under a memory's title. */
export function memorySubtitle(m: MemoryCard): string {
  switch (m.kind) {
    case 'day':
    case 'trip':
      return dayRange(m.start, m.end)
    case 'person': {
      const [a, b] = [m.start.slice(0, 4), m.end.slice(0, 4)]
      return a === b ? `${m.count} photos` : `${a}–${b}`
    }
    default:
      return `${m.count} photos`
  }
}

export const KIND_SECTIONS: { kind: MemoryKind; title: string }[] = [
  { kind: 'day', title: 'On this day' },
  { kind: 'trip', title: 'Trips' },
  { kind: 'person', title: 'People' },
  { kind: 'year', title: 'Best of the year' },
  { kind: 'month', title: 'Highlights of the month' },
]
