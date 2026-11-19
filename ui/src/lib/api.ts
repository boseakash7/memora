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
  /** Library info (favorites, albums, people…) is also kept in a hidden `.memora` folder. */
  sync: boolean
  /** The folder holding that `.memora`: this one or a parent. */
  syncRoot: string | null
  /** While keeping library info: the finished index too, so other PCs skip indexing. */
  keepIndex: boolean
  libraryInfo: LibraryInfo | null
}

/** How a folder's `.memora` is doing. */
export type LibraryInfo = {
  root: string
  /** False when this PC can't write there (read-only, unplugged): changes wait on this PC. */
  writable: boolean
  /** Other PCs that keep their info there. */
  otherPcs: string[]
  /** Changes not written to the folder yet. */
  unsent: number
  /** Size of this PC's index snapshot there (null until written, or when not kept). */
  indexBytes: number | null
}

/** What a folder holds before it is added. */
export type FolderProbe = {
  /** Folder with an existing `.memora` covering it (itself or a parent). */
  root: string | null
  /** PCs that wrote library info there. */
  pcs: string[]
}

export type Theme = 'system' | 'light' | 'dark'
/** `auto`: high with a graphics card, medium without, none when Windows animations are off. */
export type Animations = 'auto' | 'high' | 'medium' | 'none'

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
  /** File (or edit) time in seconds, used to bust cached thumbnails. */
  m: number
  /** Present (true) for favorites. */
  f?: boolean
}

/** Which items a grid shows, and how they are filtered. */
export type TimelineQuery = {
  scope?: 'all' | 'favorites' | 'album' | 'archive' | 'trash'
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
  /** Items showing this person (see `api.people`). */
  person?: number
}

/** Faces of one person, grouped automatically and corrected by the user. */
export type Person = {
  id: number
  /** Null until the user names them. */
  name: string | null
  hidden: boolean
  /** Visible photos and videos they appear in. */
  count: number
  /** Face to show for them (see `faceUrl`). */
  coverFace: number | null
}

/** A face in a photo; the box is in fractions of the displayed picture. */
export type Face = {
  id: number
  x: number
  y: number
  w: number
  h: number
  person: { id: number; name: string | null; hidden: boolean } | null
}

/** Face and content recognition, which runs after indexing. */
export type AiStatus = {
  enabled: boolean
  state: 'starting' | 'off' | 'noModels' | 'waiting' | 'working' | 'idle' | 'error'
  device: 'gpu' | 'cpu' | null
  error: string | null
  modelsDir: string | null
  /** Folders searched for the model files. */
  searched: string[]
  /** Items still to look at. */
  pending: number
  /** Which features' models are installed. */
  installed: { faces: boolean; search: boolean; blur: boolean }
}

/** Something the user can download models for. */
export type Feature = 'faces' | 'search' | 'blur'

export type FeatureStatus = {
  feature: Feature
  installed: boolean
  /** Downloaded by Memora (not part of the installed app), so it can be removed. */
  removable: boolean
  /** Bytes on disk when installed, else bytes to download. */
  size: number
  /** Blur only: the full-size model for a graphics card, not the lighter one. */
  full: boolean
  job: {
    state: 'queued' | 'downloading' | 'paused' | 'failed'
    got: number
    total: number
    error: string | null
  } | null
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
  trashed: boolean
  /** In the Archive: shown only on the Archive page. */
  archived: boolean
  stage: number
  error: string | null
  /** The saved edit; missing for the original. */
  edit?: import('./edit').Edit
  /** When last edited or reverted (unix ms). */
  editedAt: number | null
  /** Size after the edit's crop and turns. */
  editWidth: number | null
  editHeight: number | null
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
  /** Of `pending`, items at each indexing step (the whole library goes through one step
   * at a time, so `pending` only falls in the last one). */
  toRead: number
  toThumbnail: number
  toHash: number
  total: number
  /** Of `total`: photos, videos, and their size on disk in bytes. */
  photos: number
  videos: number
  bytes: number
  errors: number
  /** Items waiting for face and content recognition. */
  aiPending: number
  revision: number
}

export type Settings = {
  deviceName: string
  theme: Theme
  animations: Animations
  lanSharing: boolean
  lanPort: number
  /** While sharing: the web page for TV browsers and phones. */
  lanTvPage: boolean
  /** While sharing: the DLNA media server TVs list as a source (no PIN possible). */
  lanDlna: boolean
  /** While sharing: an Archive row on the TV page and an Archive folder for DLNA. */
  lanArchive: boolean
  tvPin: string | null
  transcodeVideos: boolean
  mlEnabled: boolean
  mlUseGpu: boolean
  /** Lock the locked folder after this many minutes without using it (1-60). */
  vaultLockMinutes: number
  /** Also lock it when Memora is minimized. */
  vaultLockOnMinimize: boolean
  /** Memories above the timeline and on their own page. */
  memories: boolean
  /** The first-start welcome was finished or skipped. */
  welcomeDone: boolean
}

/** The locked folder: set up or not, locked or not. */
export type VaultState = { exists: boolean; unlocked: boolean; count: number | null }

/** An item in the locked folder (all of this is encrypted on disk). */
export type VaultItem = {
  id: string
  name: string
  originalPath: string
  kind: 'photo' | 'video'
  mime: string
  size: number
  mtime: number
  width: number | null
  height: number | null
  durationMs: number | null
  takenAt: string
  tzOffset: number | null
  camera: string | null
  lat: number | null
  lon: number | null
  favorite: boolean
  hash: string
  addedAt: string
  hasDisplay: boolean
}

/** What a move into or out of the locked folder did. */
export type VaultMoved = { moved: number; failed: string[]; paths?: string[] }

export type MemoryKind = 'day' | 'trip' | 'person' | 'year' | 'month'

/** A memory card. */
export type MemoryCard = {
  id: number
  kind: MemoryKind
  title: string
  /** First and last day, `YYYY-MM-DD`. */
  start: string
  end: string
  coverId: number | null
  coverM: number | null
  count: number
  seen: boolean
  hidden: boolean
}

export type Memory = MemoryCard & { items: TimelineItem[] }

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

/** What a backup file holds. */
export type BackupSummary = {
  created: string
  pc: string
  folders: string[]
  favorites: number
  archived: number
  trashed: number
  albums: number
  people: number
}

/** What restoring a backup did. */
export type Restored = {
  items: number
  notFound: number
  favorites: number
  archived: number
  trashed: number
  albumsCreated: number
  albumItems: number
  peopleNamed: number
  faces: number
  facesWaiting: number
  settings: boolean
  foldersAdded: number
}

export const api = {
  folders: {
    list: () => request<Folder[]>('GET', '/folders'),
    add: (path: string, keepInfo: boolean) => request<Folder>('POST', '/folders', { path, keepInfo }),
    /** Before adding: does the folder already have library info from other PCs? */
    probe: (path: string) => request<FolderProbe>('POST', '/folders/probe', { path }),
    /** Start or stop keeping library info in the folder's `.memora`. */
    setKeepInfo: (id: number, keepInfo: boolean) => request<Folder>('PATCH', `/folders/${id}`, { keepInfo }),
    /** Keep the finished index there too, or stop (this PC's copy is deleted). */
    setKeepIndex: (id: number, keepIndex: boolean) => request<Folder>('PATCH', `/folders/${id}`, { keepIndex }),
    /** Stop, and move the folder's `.memora` (every PC's info) to the Recycle Bin. */
    removeInfo: (id: number) => request<Folder>('DELETE', `/folders/${id}/info`),
    remove: (id: number) => request<void>('DELETE', `/folders/${id}`),
    /** The folder moved or its drive letter changed: point it at its new place. */
    locate: (id: number, path: string) => request<Folder>('POST', `/folders/${id}/locate`, { path }),
  },
  settings: {
    get: () => request<Settings>('GET', '/settings'),
    update: (patch: Partial<Settings>) => request<Settings>('PATCH', '/settings', patch),
  },
  library: {
    status: () => request<LibraryStatus>('GET', '/library/status'),
    rescan: () => request<void>('POST', '/library/rescan'),
    /** Write and read library info (`.memora`) now. */
    sync: () => request<{ changed: boolean }>('POST', '/library/sync'),
  },
  backup: {
    /** Write everything you made (favorites, albums, people…) to one file. */
    write: (path: string) => request<BackupSummary>('POST', '/backup', { path }),
    read: (path: string) => request<BackupSummary>('POST', '/backup/read', { path }),
    restore: (path: string, settings: boolean) => request<Restored>('POST', '/backup/restore', { path, settings }),
  },
  timeline: (query: TimelineQuery = {}) => request<TimelineItem[]>('GET', `/timeline${queryString(query)}`),
  cameras: () => request<string[]>('GET', '/cameras'),
  media: (id: number) => request<Media>('GET', `/media/${id}`),
  mediaInfo: (id: number) => request<MediaInfo>('GET', `/media/${id}/info`),
  items: {
    favorite: (ids: number[], favorite: boolean) => request<void>('POST', '/media/favorite', { ids, favorite }),
    trash: (ids: number[]) => request<void>('POST', '/media/trash', { ids }),
    restore: (ids: number[]) => request<void>('POST', '/media/restore', { ids }),
    archive: (ids: number[]) => request<void>('POST', '/media/archive', { ids }),
    unarchive: (ids: number[]) => request<void>('POST', '/media/unarchive', { ids }),
    /** Send trashed files to the Windows Recycle Bin. All trashed items when `ids` is left out. */
    emptyTrash: (ids?: number[]) => request<Deleted>('POST', '/trash/empty', { ids }),
    albums: (id: number) => request<AlbumRef[]>('GET', `/media/${id}/albums`),
    /** File paths, in the order asked. */
    paths: (ids: number[]) => request<string[]>('POST', '/media/paths', { ids }),
  },
  places: {
    list: () => request<Place[]>('GET', '/places'),
    get: (id: number) => request<Place>('GET', `/places/${id}`),
  },
  people: {
    list: (hidden = false) => request<Person[]>('GET', `/people${hidden ? '?hidden=true' : ''}`),
    get: (id: number) => request<Person>('GET', `/people/${id}`),
    update: (id: number, patch: { name?: string | null; hidden?: boolean; coverFace?: number }) =>
      request<Person>('PATCH', `/people/${id}`, patch),
    /** Fold the `from` people into `id`. */
    merge: (id: number, from: number[]) => request<Person>('POST', `/people/${id}/merge`, { from }),
  },
  faces: {
    of: (mediaId: number) => request<Face[]>('GET', `/media/${mediaId}/faces`),
    /** Put a face with a person; null means "not this person". */
    setPerson: (faceId: number, person: number | null) =>
      request<Person | null>('PUT', `/faces/${faceId}/person`, { person }),
    newPerson: (faceId: number, newName: string) => request<Person>('PUT', `/faces/${faceId}/person`, { newName }),
  },
  ai: {
    status: () => request<AiStatus | null>('GET', '/ai'),
    /** Look for the model files again. */
    refresh: () => request<AiStatus | null>('POST', '/ai/refresh'),
    features: () => request<FeatureStatus[]>('GET', '/ai/features'),
    download: (f: Feature) => request<FeatureStatus[]>('POST', `/ai/features/${f}/download`),
    /** Stop, keeping what came down so it can go on later. */
    pause: (f: Feature) => request<FeatureStatus[]>('POST', `/ai/features/${f}/pause`),
    /** Stop and throw away what came down. */
    cancel: (f: Feature) => request<FeatureStatus[]>('POST', `/ai/features/${f}/cancel`),
    remove: (f: Feature) => request<FeatureStatus[]>('DELETE', `/ai/features/${f}`),
  },
  lan: () => request<LanStatus>('GET', '/lan'),
  mapPoints: () => request<MapPoint[]>('GET', '/map/points'),
  duplicates: () => request<DupGroup[]>('GET', '/duplicates'),
  vault: {
    state: () => request<VaultState>('GET', '/vault'),
    /** Returns the recovery key, shown once. Leaves it unlocked. */
    setup: (password: string) => request<{ recoveryKey: string }>('POST', '/vault/setup', { password }),
    unlock: (password: string) => request<VaultState>('POST', '/vault/unlock', { password }),
    /** Forgot the password: unlock with the recovery key and set a new password. */
    recover: (recoveryKey: string, password: string) =>
      request<VaultState>('POST', '/vault/recover', { recoveryKey, password }),
    lock: () => request<VaultState>('POST', '/vault/lock'),
    changePassword: (old: string, next: string) => request<void>('POST', '/vault/password', { old, new: next }),
    newRecoveryKey: (password: string) => request<{ recoveryKey: string }>('POST', '/vault/recovery-key', { password }),
    items: () => request<VaultItem[]>('GET', '/vault/items'),
    moveIn: (ids: number[]) => request<VaultMoved>('POST', '/vault/move-in', { ids }),
    moveOut: (ids: string[], path: string) => request<VaultMoved>('POST', '/vault/move-out', { ids, path }),
    delete: (ids: string[]) => request<{ deleted: number }>('POST', '/vault/delete', { ids }),
    setFavorite: (ids: string[], favorite: boolean) =>
      request<{ changed: number }>('POST', '/vault/favorite', { ids, favorite }),
  },
  memories: {
    list: (hidden = false) => request<MemoryCard[]>('GET', `/memories${hidden ? '?hidden=true' : ''}`),
    get: (id: number) => request<Memory>('GET', `/memories/${id}`),
    seen: (id: number) => request<void>('POST', `/memories/${id}/seen`),
    setHidden: (id: number, hidden: boolean) => request<Memory>('PATCH', `/memories/${id}`, { hidden }),
    remove: (id: number, ids: number[]) => request<Memory>('POST', `/memories/${id}/remove`, { ids }),
    /** Work them out again now (normally done in the background). */
    rebuild: () => request<void>('POST', '/memories/rebuild'),
  },
  edit: {
    /** `subjectsGpu`: background blur's subject finder runs on a graphics card (null: not known yet). */
    filters: () =>
      request<{ filters: import('./edit').FilterInfo[]; subjects: boolean; subjectsGpu: boolean | null }>(
        'GET',
        '/edit/filters',
      ),
    get: (id: number) => request<{ edit: import('./edit').Edit | null; editedAt: number | null }>('GET', `/media/${id}/edit`),
    /** Save (null: back to the original). Returns the new size and picture version. */
    save: (id: number, edit: import('./edit').Edit | null) =>
      request<{ edit: import('./edit').Edit | null; editedAt: number | null; w: number; h: number; m: number }>(
        'PUT',
        `/media/${id}/edit`,
        { edit },
      ),
    enhance: (id: number, edit: import('./edit').Edit) =>
      request<{ adjust: import('./edit').Adjust; scene: import('./edit').Scene | null; suggested: string[] }>(
        'POST',
        `/media/${id}/edit/enhance`,
        { edit },
      ),
    /** Find the main subject, for background blur (a few seconds the first time). */
    subject: (id: number) => request<{ ok: boolean }>('POST', `/media/${id}/edit/subject`),
    export: (id: number, path: string, edit?: import('./edit').Edit) =>
      request<{ path: string; width: number; height: number }>('POST', `/media/${id}/edit/export`, { path, edit }),
  },
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

/** A video converted while it plays, starting `t` seconds in (the stream's own clock starts at 0). */
export function streamUrl(id: number, t: number): string {
  const url = mediaUrl(id, 'stream.mp4')
  return t > 0 ? `${url}&t=${t.toFixed(3)}` : url
}

/** The video frame at `t` seconds, as a full-size PNG. */
export function frameUrl(id: number, t: number): string {
  if (!resolved) throw new Error('server info not loaded yet')
  return `${resolved.baseUrl}/api/media/${id}/frame?t=${t.toFixed(3)}&token=${resolved.token}`
}

/** Small frames of a video in one picture, for the seek bar's preview. Tile `i` shows `i * interval` s. */
export type Storyboard = { url: string; frames: number; interval: number; cols: number; rows: number }

/**
 * Loads a video's storyboard (the PC makes it on first use, which takes a second or two).
 * The returned `url` is an object URL; revoke it when done.
 */
export async function loadStoryboard(id: number, version: number, signal?: AbortSignal): Promise<Storyboard> {
  if (!resolved) throw new Error('server info not loaded yet')
  const res = await fetch(`${resolved.baseUrl}/api/media/${id}/storyboard?v=${version}&token=${resolved.token}`, { signal })
  if (!res.ok) throw new ApiError(res.status, `storyboard: ${res.status}`)
  const layout = Object.fromEntries(
    (res.headers.get('x-storyboard') ?? '').split(';').map((kv) => {
      const [k, v] = kv.split('=')
      return [k, Number(v)]
    }),
  )
  const { frames, interval, cols, rows } = layout
  if (!(frames > 0 && interval > 0 && cols > 0 && rows > 0)) throw new Error('storyboard: no layout')
  return { url: URL.createObjectURL(await res.blob()), frames, interval, cols, rows }
}

/** A locked-folder file, decrypted on the fly (never cached by the webview). */
export function vaultUrl(id: string, part: 'thumb' | 'display' | 'original'): string {
  if (!resolved) throw new Error('server info not loaded yet')
  return `${resolved.baseUrl}/api/vault/items/${id}/${part}?token=${resolved.token}`
}

/** Square picture of a face. */
export function faceUrl(faceId: number): string {
  if (!resolved) throw new Error('server info not loaded yet')
  return `${resolved.baseUrl}/api/faces/${faceId}/thumb?token=${resolved.token}`
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

/** Where the library data (database + cache) is. */
export type DataFolder = { dataDir: string; size: number; moved: boolean }
/** Where a move to the picked folder would go. */
export type DataTarget = { target: string; size: number; free: number }
export type DataMoveProgress = { phase: 'copy' | 'finish'; done: number; total: number }
/** A failed move; `restart` when background work was already stopped. */
export type DataMoveError = { message: string; restart: boolean }

export const dataFolder = {
  get: () => invoke<DataFolder>('data_folder'),
  /** Rejects with the reason when the folder can't hold the data. */
  target: (path: string) => invoke<DataTarget>('data_folder_target', { path }),
  /** Copies, switches and restarts the app; rejects with a `DataMoveError`. */
  move: (path: string) => invoke<void>('move_data_folder', { path }),
  restart: () => invoke<void>('restart_app'),
}

/** Empty the webview's disk cache (after items moved to the locked folder, so no copy of
 *  their pictures stays there). Best effort; nothing to do in the dev browser. */
export async function clearWebCache(): Promise<void> {
  try {
    await invoke('clear_web_cache')
  } catch {
    // Not in the app, or the webview refused; the cache expires on its own.
  }
}

/** Make a photo the Windows desktop background. */
export function setWallpaper(id: number): Promise<void> {
  return invoke('set_wallpaper', { id })
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
