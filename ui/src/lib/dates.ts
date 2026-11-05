/** Dates in the library are local wall-clock strings: `YYYY-MM-DDTHH:MM:SS`. */
export function parseLocal(t: string): Date {
  const [y, mo, d, h = 0, mi = 0, s = 0] = t.split(/[-T:]/).map(Number)
  return new Date(y, mo - 1, d, h, mi, s)
}

function sameDay(a: Date, b: Date) {
  return a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate()
}

/** Section heading for a day: "Today", "Yesterday", "Sat, 12 Oct" or "Sat, 12 Oct 2023". */
export function dayLabel(day: string, now = new Date()): string {
  const date = parseLocal(day)
  if (sameDay(date, now)) return 'Today'
  const yesterday = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1)
  if (sameDay(date, yesterday)) return 'Yesterday'
  return date.toLocaleDateString(undefined, {
    weekday: 'short',
    day: 'numeric',
    month: 'short',
    year: date.getFullYear() === now.getFullYear() ? undefined : 'numeric',
  })
}

export function monthLabel(year: number, month: number): string {
  return new Date(year, month - 1, 1).toLocaleDateString(undefined, { month: 'long', year: 'numeric' })
}

export function shortMonth(month: number): string {
  return new Date(2000, month - 1, 1).toLocaleDateString(undefined, { month: 'short' })
}

export function dateTimeLabel(t: string): { date: string; time: string } {
  const d = parseLocal(t)
  return {
    date: d.toLocaleDateString(undefined, { weekday: 'short', day: 'numeric', month: 'short', year: 'numeric' }),
    time: d.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' }),
  }
}

/** `0:42`, `3:05`, `1:02:03` */
export function formatDuration(ms: number): string {
  const total = Math.round(ms / 1000)
  const h = Math.floor(total / 3600)
  const m = Math.floor((total % 3600) / 60)
  const s = String(total % 60).padStart(2, '0')
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${s}` : `${m}:${s}`
}

export function formatBytes(n: number): string {
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let i = 0
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024
    i++
  }
  return `${n.toFixed(i === 0 || n >= 100 ? 0 : 1)} ${units[i]}`
}

/** `8.1 Mbps`, `640 kbps` */
export function formatBitrate(bps: number): string {
  return bps >= 1e6 ? `${(bps / 1e6).toFixed(1)} Mbps` : `${Math.round(bps / 1e3)} kbps`
}

const CODECS: Record<string, string> = {
  h264: 'H.264',
  hevc: 'HEVC (H.265)',
  mpeg4: 'MPEG-4',
  mpeg2video: 'MPEG-2',
  wmv3: 'WMV',
  vp8: 'VP8',
  vp9: 'VP9',
  av1: 'AV1',
  aac: 'AAC',
  mp3: 'MP3',
  ac3: 'AC-3',
  eac3: 'E-AC-3',
  opus: 'Opus',
  vorbis: 'Vorbis',
  flac: 'FLAC',
}

/** FFmpeg codec name as people write it: `h264` becomes `H.264`. */
export function codecLabel(name: string): string {
  return CODECS[name] ?? name.toUpperCase()
}

export function formatOffset(minutes: number): string {
  const sign = minutes < 0 ? '-' : '+'
  const abs = Math.abs(minutes)
  return `GMT${sign}${String(Math.floor(abs / 60)).padStart(2, '0')}:${String(abs % 60).padStart(2, '0')}`
}
