import { invoke } from '@tauri-apps/api/core'

/** Address and access token of the server embedded in the desktop app. */
export type ServerInfo = {
  baseUrl: string
  token: string
  dataDir: string
  version: string
}

export type Folder = {
  id: number
  path: string
  enabled: boolean
  addedAt: string
  available: boolean
}

export type Theme = 'system' | 'light' | 'dark'

/** One grid cell, newest first. Short keys keep big timelines small. */
export type TimelineItem = {
  id: number
  /** Width and height as displayed; 0 when unknown. */
  w: number
  h: number
  /** Taken at, local time `YYYY-MM-DDTHH:MM:SS`. */
  t: string
  /** Present (true) for videos. */
  v?: boolean
  /** Video duration in ms. */
  d?: number
  /** File mtime in seconds, used to bust cached thumbnails. */
  m: number
  /** Present (true) for favorites. */
  f?: boolean
}

/** Which items a grid shows, and how they are filtered. */
export type TimelineQuery = {
  scope?: 'all' | 'favorites' | 'album' | 'trash'
  album?: number
  /** Free text: words must match the start of words in file/folder names, cameras and album names. */
  q?: string
  /** First and last day to include, `YYYY-MM-DD`. */
  from?: string
  to?: string
  kind?: 'photo' | 'video'
  camera?: string
  /** Items taken at this place (see `api.places`). */
  place?: number
  /** Items with GPS inside this box: `west,south,east,north` in degrees. */
  bbox?: string
}

export type Album = {
  id: number
  name: string
  createdAt: string
  /** Visible items (not in the trash). */
  count: number
  coverId: number | null
  /** Modification time of the cover file in seconds, for the thumbnail URL. */
  coverM: number | null
}

export type AlbumRef = { id: number; name: string }

export type Place = {
  id: number
  /** ISO 3166-1 alpha-2 code, e.g. `IN`; see `countryName`. */
  country: string
  admin1: string
  city: string
  count: number
  coverId: number | null
  coverM: number | null
}

/** A geotagged item: `[id, latitude, longitude]`. */
export type MapPoint = [number, number, number]

export type DupItem = {
  id: number
  name: string
  path: string
  w: number
  h: number
  size: number
  takenAt: string
  m: number
  video: boolean
  durationMs: number | null
  /** The suggested copy to keep. */
  keeper: boolean
}

export type DupGroup = {
  /** `exact`: identical files. `near`: look the same. */
  kind: 'exact' | 'near'
  /** The suggested keeper comes first. */
  items: DupItem[]
  /** Bytes freed by removing all but the keeper. */
  reclaimable: number
}

/** Result of emptying the trash. */
export type Deleted = { deleted: number; failed: string[] }

export type Media = {
  id: number
  folderId: number
  path: string
  name: string
  size: number
  mtime: number
  kind: 'photo' | 'video'
  mime: string
  width: number | null
  height: number | null
  durationMs: number | null
  orientation: number
  takenAt: string
  tzOffset: number | null
  cameraMake: string | null
  cameraModel: string | null
  lat: number | null
  lon: number | null
  favorite: boolean
  missing: boolean
  stage: number
  error: string | null
}

/** Read from the file when the info panel opens; every field is null when the file doesn't say. */
export type MediaInfo = {
  lens: string | null
  iso: number | null
  fNumber: number | null
  /** Shutter speed in seconds, as a camera shows it: `1/250` or `2.5`. */
  exposure: string | null
  /** Millimetres. */
  focalLength: number | null
  flash: boolean | null
  software: string | null
  /** Metres above sea level. */
  altitude: number | null
  videoCodec: string | null
  audioCodec: string | null
  fps: number | null
  /** Bits per second. */
  bitrate: number | null
  place: { country: string; admin1: string; city: string } | null
}

export type LibraryStatus = {
  scanning: boolean
  scanFolder: string | null
  scannedFiles: number
  pending: number
  total: number
  errors: number
  revision: number
}

export type Settings = {
  deviceName: string
  theme: Theme
  lanSharing: boolean
  lanPort: number
  /** While sharing: the web page for TV browsers and phones. */
  lanTvPage: boolean
  /** While sharing: the DLNA media server TVs list as a source (no PIN possible). */
  lanDlna: boolean
  tvPin: string | null
  transcodeVideos: boolean
  mlEnabled: boolean
  mlUseGpu: boolean
}

/** Network sharing as it is running now. */
export type LanStatus = {
  running: boolean
  port: number
  tvPage: boolean
  dlna: boolean
  /** Why sharing isn't running although it's switched on (e.g. port in use). */
  error: string | null
  addresses: { name: string; ip: string; isVirtual: boolean }[]
}

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message)
  }
}

let info: Promise<ServerInfo> | null = null
let resolved: ServerInfo | null = null

export function serverInfo(): Promise<ServerInfo> {
  info ??= invoke<ServerInfo>('server_info').then((i) => (resolved = i))
  return info
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const { baseUrl, token } = await serverInfo()
  const headers: Record<string, string> = { Authorization: `Bearer ${token}` }
  if (body !== undefined) headers['Content-Type'] = 'application/json'

  const res = await fetch(`${baseUrl}/api${path}`, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  if (!res.ok) {
    let message = res.statusText
    try {
      message = (await res.json()).error ?? message
    } catch {
      // Not JSON; keep the status text.
    }
    throw new ApiError(res.status, message)
  }
  return (res.status === 204 ? undefined : await res.json()) as T
}

export const api = {
  folders: {
    list: () => request<Folder[]>('GET', '/folders'),
    add: (path: string) => request<Folder>('POST', '/folders', { path }),
    remove: (id: number) => request<void>('DELETE', `/folders/${id}`),
  },
  settings: {
    get: () => request<Settings>('GET', '/settings'),
    update: (patch: Partial<Settings>) => request<Settings>('PATCH', '/settings', patch),
  },
  library: {
    status: () => request<LibraryStatus>('GET', '/library/status'),
    rescan: () => request<void>('POST', '/library/rescan'),
  },
  timeline: (query: TimelineQuery = {}) => request<TimelineItem[]>('GET', `/timeline${queryString(query)}`),
  cameras: () => request<string[]>('GET', '/cameras'),
  media: (id: number) => request<Media>('GET', `/media/${id}`),
  mediaInfo: (id: number) => request<MediaInfo>('GET', `/media/${id}/info`),
  items: {
    favorite: (ids: number[], favorite: boolean) => request<void>('POST', '/media/favorite', { ids, favorite }),
    trash: (ids: number[]) => request<void>('POST', '/media/trash', { ids }),
    restore: (ids: number[]) => request<void>('POST', '/media/restore', { ids }),
    /** Send trashed files to the Windows Recycle Bin. All trashed items when `ids` is left out. */
    emptyTrash: (ids?: number[]) => request<Deleted>('POST', '/trash/empty', { ids }),
    albums: (id: number) => request<AlbumRef[]>('GET', `/media/${id}/albums`),
  },
  places: {
    list: () => request<Place[]>('GET', '/places'),
    get: (id: number) => request<Place>('GET', `/places/${id}`),
  },
  lan: () => request<LanStatus>('GET', '/lan'),
  mapPoints: () => request<MapPoint[]>('GET', '/map/points'),
  duplicates: () => request<DupGroup[]>('GET', '/duplicates'),
  albums: {
    list: () => request<Album[]>('GET', '/albums'),
    get: (id: number) => request<Album>('GET', `/albums/${id}`),
    create: (name: string, ids: number[] = []) => request<Album>('POST', '/albums', { name, ids }),
    update: (id: number, patch: { name?: string; coverMediaId?: number }) =>
      request<Album>('PATCH', `/albums/${id}`, patch),
    remove: (id: number) => request<void>('DELETE', `/albums/${id}`),
    addItems: (id: number, ids: number[]) => request<void>('POST', `/albums/${id}/items`, { ids }),
    removeItems: (id: number, ids: number[]) => request<void>('DELETE', `/albums/${id}/items`, { ids }),
  },
}

function queryString(query: TimelineQuery): string {
  const params = new URLSearchParams()
  for (const [key, value] of Object.entries(query)) {
    if (value !== undefined && value !== '') params.set(key, String(value))
  }
  const s = params.toString()
  return s ? `?${s}` : ''
}

/**
 * URL for `<img>` / `<video>` elements, which can't send headers, so the token goes
 * in the query. Only valid once `serverInfo()` has resolved (any API call does that).
 */
/**
 * Bump when thumbnails must be regenerated without the files changing (e.g. a decoding
 * fix), so the webview doesn't keep showing its cached copies.
 * 2: tiled HEIC photos are stitched instead of showing their first tile.
 */
const THUMB_REVISION = 2

export function mediaUrl(id: number, what: 'thumb' | 'display' | 'original' | 'stream.mp4', version?: number): string {
  if (!resolved) throw new Error('server info not loaded yet')
  const v = version === undefined ? '' : `v=${version}.${THUMB_REVISION}&`
  return `${resolved.baseUrl}/api/media/${id}/${what}?${v}token=${resolved.token}`
}

/** QR code image (SVG) for a short text such as a URL. */
export function qrUrl(text: string): string {
  if (!resolved) throw new Error('server info not loaded yet')
  return `${resolved.baseUrl}/api/lan/qr.svg?text=${encodeURIComponent(text)}&token=${resolved.token}`
}

export function openWithDefaultApp(path: string): Promise<void> {
  return invoke('open_with_default_app', { path })
}

export function showInExplorer(path: string): Promise<void> {
  return invoke('show_in_explorer', { path })
}

/** "India" for `IN`, using the names built into the webview. Falls back to the code. */
export function countryName(code: string): string {
  try {
    return new Intl.DisplayNames(undefined, { type: 'region' }).of(code) ?? code
  } catch {
    return code
  }
}

export function errorMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}
