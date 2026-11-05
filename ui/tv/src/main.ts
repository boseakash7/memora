// The TV page: browse and show the library on a TV browser or phone, with the remote.
//
// Plain DOM code (no framework) for old TV browsers. Screens are a stack (home, grid,
// viewer); each opened screen adds a browser history entry, so the remote's Back key (which
// TV browsers turn into "history back") closes it.
//
// Remote: arrows move between tiles, OK opens. In the viewer: left/right = previous/next
// (seek for videos), OK = slideshow on/off (play/pause for videos), up = info,
// down = slideshow speed (progress for videos), CH+/CH- or media keys = next/previous.

import './tv.css'

type Item = { id: number; w: number; h: number; t: string; v?: boolean; d?: number; m: number; f?: boolean }
type Album = { id: number; name: string; count: number; coverId: number | null; coverM: number | null }
type Place = {
  id: number
  country: string
  countryName: string
  admin1: string
  city: string
  count: number
  coverId: number | null
  coverM: number | null
}
type Home = {
  name: string
  total: number
  recent: Item[]
  favorites: Item[]
  videos: Item[]
  albums: Album[]
  places: Place[]
  transcode: boolean
}
type Details = {
  id: number
  name: string
  video: boolean
  takenAt: string
  durationMs: number | null
  videoCodec: string | null
  audioCodec: string | null
  mime: string
  canConvert: boolean
}
type Hello = { name: string; pinRequired: boolean; authorized: boolean }
type ListSpec = { scope: 'all' | 'favorites' | 'videos' | 'album' | 'place'; id?: number; title: string }

const app = document.getElementById('app') as HTMLElement

// ---------- server ----------

class HttpError extends Error {
  status: number
  constructor(status: number, message: string) {
    super(message)
    this.status = status
  }
}

async function call<T>(method: string, path: string, body?: unknown): Promise<T> {
  const init: RequestInit = { method, credentials: 'same-origin' }
  if (body !== undefined) {
    init.body = JSON.stringify(body)
    init.headers = { 'Content-Type': 'application/json' }
  }
  const res = await fetch('/tv/api' + path, init)
  if (!res.ok) {
    let message = res.statusText
    try {
      message = (await res.json()).error || message
    } catch (_) {
      // not JSON
    }
    if (res.status === 401 && path !== '/login') {
      // The PIN changed while we were open: start over at the PIN screen.
      location.reload()
    }
    throw new HttpError(res.status, message)
  }
  return (res.status === 204 ? undefined : await res.json()) as T
}

const get = <T>(path: string) => call<T>('GET', path)

const fileUrl = (id: number, file: string, version?: number) =>
  '/tv/api/media/' + id + '/' + file + (version === undefined ? '' : '?v=' + version)

const lists: { [key: string]: Item[] } = {}

async function loadList(spec: ListSpec): Promise<Item[]> {
  const key = spec.scope + ':' + (spec.id || '')
  if (!lists[key]) {
    lists[key] = await get<Item[]>('/items?scope=' + spec.scope + (spec.id ? '&id=' + spec.id : ''))
  }
  return lists[key]
}

// ---------- DOM helpers ----------

type Child = Node | string | null | undefined | false

function h(tag: string, props?: { [key: string]: unknown } | null, ...children: Child[]): HTMLElement {
  const el = document.createElement(tag)
  if (props) {
    for (const key of Object.keys(props)) {
      const value = props[key]
      if (key.indexOf('on') === 0 && typeof value === 'function') {
        el.addEventListener(key.slice(2), value as EventListener)
      } else if (value !== undefined && value !== null && value !== false) {
        ;(el as unknown as { [key: string]: unknown })[key] = value
      }
    }
  }
  for (const c of children) {
    if (c !== null && c !== undefined && c !== false) el.appendChild(typeof c === 'string' ? document.createTextNode(c) : c)
  }
  return el
}

const pad2 = (n: number) => (n < 10 ? '0' : '') + n

const MONTHS = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December']

/** `2024-03-05T10:07:00` as "5 March 2024, 10:07". */
function formatDate(t: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})/.exec(t)
  if (!m) return t
  return parseInt(m[3], 10) + ' ' + MONTHS[parseInt(m[2], 10) - 1] + ' ' + m[1] + ', ' + m[4] + ':' + m[5]
}

function formatDuration(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds))
  const hours = Math.floor(s / 3600)
  const rest = pad2(Math.floor(s / 60) % 60) + ':' + pad2(s % 60)
  return hours > 0 ? hours + ':' + rest : String(Math.floor(s / 60)) + ':' + pad2(s % 60)
}

// Thumbnails load when they come near the screen.
const observer =
  'IntersectionObserver' in window
    ? new IntersectionObserver(
        (entries) => {
          for (const e of entries) {
            if (e.isIntersecting) {
              const img = e.target as HTMLImageElement
              img.src = img.getAttribute('data-src') || ''
              observer!.unobserve(img)
            }
          }
        },
        { rootMargin: '400px' },
      )
    : null

function lazyImg(src: string): HTMLImageElement {
  const img = h('img', { alt: '' }) as HTMLImageElement
  if (observer) {
    img.setAttribute('data-src', src)
    observer.observe(img)
  } else {
    img.src = src
  }
  return img
}

// ---------- remote keys ----------

type Key = 'left' | 'right' | 'up' | 'down' | 'enter' | 'back' | 'playpause' | 'play' | 'pause' | 'ff' | 'rw' | 'next' | 'prev' | 'delete' | null

function keyOf(e: KeyboardEvent): Key {
  switch (e.keyCode) {
    case 37: return 'left'
    case 38: return 'up'
    case 39: return 'right'
    case 40: return 'down'
    case 13: return 'enter'
    case 8: return 'delete' // Backspace: deletes a PIN digit, else goes back
    case 27: case 10009: case 461: case 166: return 'back' // Esc, Samsung, LG, browser back
    case 10252: case 179: return 'playpause'
    case 415: case 250: return 'play'
    case 19: return 'pause'
    case 417: case 228: return 'ff'
    case 412: case 227: return 'rw'
    case 427: case 33: case 176: return 'next' // CH+, Page Up, next track
    case 428: case 34: case 177: return 'prev' // CH-, Page Down, previous track
  }
  if (e.key === 'MediaPlayPause') return 'playpause'
  if (e.key === 'GoBack' || e.key === 'BrowserBack') return 'back'
  return null
}

/** Ask a Samsung TV to send media keys to the page (no-op elsewhere). */
function registerTvKeys() {
  const w = window as unknown as { tizen?: { tvinputdevice?: { registerKey(k: string): void } } }
  const input = w.tizen && w.tizen.tvinputdevice
  if (!input) return
  for (const k of ['MediaPlayPause', 'MediaPlay', 'MediaPause', 'MediaFastForward', 'MediaRewind', 'ChannelUp', 'ChannelDown']) {
    try {
      input.registerKey(k)
    } catch (_) {
      // not allowed in the browser; arrows and OK still work
    }
  }
}

// ---------- focus ----------

function focusables(root: HTMLElement): HTMLElement[] {
  return Array.prototype.slice.call(root.querySelectorAll('.focusable'))
}

function focusEl(el: HTMLElement) {
  try {
    el.focus({ preventScroll: true })
  } catch (_) {
    el.focus()
  }
  reveal(el)
}

/** Scroll so the focused tile is comfortably on screen (rows scroll sideways). */
function reveal(el: HTMLElement) {
  const row = el.parentElement
  if (row && row.className.indexOf('row-items') >= 0) {
    const r = el.getBoundingClientRect()
    const box = row.getBoundingClientRect()
    const margin = box.width * 0.06
    if (r.left < box.left + margin) row.scrollLeft -= box.left + margin - r.left
    else if (r.right > box.right - margin) row.scrollLeft += r.right - (box.right - margin)
  }
  const r = el.getBoundingClientRect()
  const vh = window.innerHeight
  const margin = vh * 0.12
  if (r.top < margin) window.scrollBy(0, r.top - margin)
  else if (r.bottom > vh - margin) window.scrollBy(0, r.bottom - (vh - margin))
}

/** Move focus to the nearest tile in a direction. */
function moveFocus(root: HTMLElement, dir: 'left' | 'right' | 'up' | 'down') {
  const els = focusables(root)
  const cur = document.activeElement as HTMLElement | null
  if (!cur || els.indexOf(cur) < 0) {
    if (els.length) focusEl(els[0])
    return
  }
  const a = cur.getBoundingClientRect()
  const ax = (a.left + a.right) / 2
  const ay = (a.top + a.bottom) / 2
  // The focused tile is drawn slightly larger; allow for that.
  const slackX = a.width * 0.1
  const slackY = a.height * 0.1
  let best: HTMLElement | null = null
  let bestScore = Infinity
  for (const el of els) {
    if (el === cur) continue
    const b = el.getBoundingClientRect()
    if (b.width === 0 && b.height === 0) continue
    let main: number
    let cross: number
    const overlapY = b.top < a.bottom - slackY && b.bottom > a.top + slackY
    const overlapX = b.left < a.right - slackX && b.right > a.left + slackX
    if (dir === 'right' || dir === 'left') {
      main = dir === 'right' ? b.left - a.right : a.left - b.right
      if (main < -slackX) continue
      cross = overlapY ? 0 : Math.abs((b.top + b.bottom) / 2 - ay)
    } else {
      main = dir === 'down' ? b.top - a.bottom : a.top - b.bottom
      if (main < -slackY) continue
      cross = overlapX ? Math.abs(b.left - a.left) * 0.2 : Math.abs((b.left + b.right) / 2 - ax)
    }
    const score = Math.max(main, 0) + cross * 3
    if (score < bestScore) {
      bestScore = score
      best = el
    }
  }
  if (best) focusEl(best)
}

// ---------- screens ----------

interface Screen {
  el: HTMLElement
  /** Handle a key; return true if it was used. */
  onKey?: (key: Key, e: KeyboardEvent) => boolean
  onClose?: () => void
  /** Called when the screen is shown again after the one above it closed. */
  onShow?: () => void
  lastFocus?: HTMLElement | null
}

const stack: Screen[] = []

function top(): Screen | undefined {
  return stack[stack.length - 1]
}

function setRoot(screen: Screen) {
  while (stack.length) closeTop()
  app.innerHTML = ''
  history.replaceState({ depth: 0 }, '')
  stack.push(screen)
  app.appendChild(screen.el)
  window.scrollTo(0, 0)
  focusFirst(screen.el)
}

function push(screen: Screen) {
  const below = top()
  if (below) {
    below.lastFocus = document.activeElement as HTMLElement
    below.el.style.display = 'none'
  }
  stack.push(screen)
  app.appendChild(screen.el)
  history.pushState({ depth: stack.length - 1 }, '')
  window.scrollTo(0, 0)
  focusFirst(screen.el)
}

function closeTop() {
  const s = stack.pop()
  if (!s) return
  if (s.onClose) s.onClose()
  if (s.el.parentNode) s.el.parentNode.removeChild(s.el)
}

window.addEventListener('popstate', (e) => {
  const depth = e.state && typeof e.state.depth === 'number' ? e.state.depth : 0
  while (stack.length > 1 && stack.length - 1 > depth) {
    closeTop()
    const s = top()!
    s.el.style.display = ''
    if (s.onShow) s.onShow()
    if (s.lastFocus && document.body.contains(s.lastFocus)) focusEl(s.lastFocus)
    else focusFirst(s.el)
  }
})

function back() {
  if (stack.length > 1) history.back()
}

function focusFirst(root: HTMLElement) {
  const first = focusables(root)[0]
  if (first) focusEl(first)
}

document.addEventListener('keydown', (e) => {
  const key = keyOf(e)
  const s = top()
  if (!s) return
  // Screens see every key first (the PIN screen wants digits).
  if (s.onKey && s.onKey(key, e)) {
    e.preventDefault()
    return
  }
  if (!key) return
  if (key === 'left' || key === 'right' || key === 'up' || key === 'down') {
    e.preventDefault()
    moveFocus(s.el, key)
  } else if (key === 'enter') {
    // Activate the tile ourselves: not every TV browser turns OK into a click.
    const active = document.activeElement as HTMLElement | null
    if (active && active.className.indexOf('focusable') >= 0 && s.el.contains(active)) {
      e.preventDefault()
      active.click()
    }
  } else if ((key === 'back' || key === 'delete') && stack.length > 1) {
    e.preventDefault()
    back()
  }
})

// ---------- tiles ----------

function itemTile(item: Item, open: () => void): HTMLElement {
  return h(
    'button',
    { className: 'tile focusable', onclick: open },
    lazyImg(fileUrl(item.id, 'thumb.jpg', item.m)),
    item.v ? h('span', { className: 'badge' }, '▶ ' + formatDuration((item.d || 0) / 1000)) : null,
  )
}

function coverTile(coverId: number | null, coverM: number | null, title: string, sub: string, open: () => void): HTMLElement {
  return h(
    'button',
    { className: 'tile focusable', onclick: open },
    coverId !== null ? lazyImg(fileUrl(coverId, 'thumb.jpg', coverM === null ? undefined : coverM)) : null,
    h('span', { className: 'label' }, title, h('small', null, sub)),
  )
}

function moreTile(title: string, open: () => void): HTMLElement {
  return h('button', { className: 'tile more focusable', onclick: open }, title)
}

const plural = (n: number, one: string, many: string) => n.toLocaleString() + ' ' + (n === 1 ? one : many)

// ---------- home ----------

async function homeScreen(): Promise<Screen> {
  const home = await get<Home>('/home')
  document.title = home.name
  const all: ListSpec = { scope: 'all', title: 'All photos and videos' }
  const favorites: ListSpec = { scope: 'favorites', title: 'Favorites' }
  const videos: ListSpec = { scope: 'videos', title: 'Videos' }

  const row = (title: string, tiles: HTMLElement[]) =>
    tiles.length ? h('section', { className: 'row' }, h('h2', null, title), h('div', { className: 'row-items' }, ...tiles)) : null

  const el = h(
    'div',
    { className: 'home' },
    h('div', { className: 'header' }, h('h1', null, home.name), h('span', { className: 'count' }, plural(home.total, 'photo or video', 'photos and videos'))),
    home.total === 0 ? h('p', { className: 'message' }, 'Nothing to show yet. Add folders in Memora on the PC.') : null,
    row(
      'Recent',
      home.total
        ? [moreTile('See all', () => openList(all))].concat(home.recent.map((i) => itemTile(i, () => openViewer(all, i.id))))
        : [],
    ),
    row(
      'Albums',
      home.albums.map((a) => coverTile(a.coverId, a.coverM, a.name, plural(a.count, 'item', 'items'), () => openList({ scope: 'album', id: a.id, title: a.name }))),
    ),
    row(
      'Places',
      home.places.map((p) => {
        const name = p.city || p.admin1 || p.countryName
        return coverTile(p.coverId, p.coverM, name, p.countryName + ' · ' + plural(p.count, 'item', 'items'), () =>
          openList({ scope: 'place', id: p.id, title: name }),
        )
      }),
    ),
    row(
      'Favorites',
      home.favorites.length ? [moreTile('See all', () => openList(favorites))].concat(home.favorites.map((i) => itemTile(i, () => openViewer(favorites, i.id)))) : [],
    ),
    row(
      'Videos',
      home.videos.length ? [moreTile('See all', () => openList(videos))].concat(home.videos.map((i) => itemTile(i, () => openViewer(videos, i.id)))) : [],
    ),
  )
  return { el }
}

// ---------- grid ----------

const GRID_BATCH = 120

async function openList(spec: ListSpec) {
  const items = await loadList(spec).catch(showError)
  if (!items) return
  const grid = h('div', { className: 'grid' })
  const el = h('div', { className: 'grid-page' }, h('h1', null, spec.title, h('small', null, plural(items.length, 'item', 'items'))), grid)
  let shown = 0
  const more = () => {
    const end = Math.min(items.length, shown + GRID_BATCH)
    for (let i = shown; i < end; i++) {
      const item = items[i]
      grid.appendChild(itemTile(item, () => openViewer(spec, item.id)))
    }
    shown = end
  }
  more()
  // Add more tiles as focus or scrolling nears the end.
  const nearEnd = () => {
    if (shown >= items.length) return
    const last = grid.lastElementChild as HTMLElement | null
    if (last && last.getBoundingClientRect().top < window.innerHeight * 2) more()
  }
  window.addEventListener('scroll', nearEnd)
  el.addEventListener('focusin', nearEnd)
  push({
    el,
    onClose: () => window.removeEventListener('scroll', nearEnd),
  })
}

// ---------- viewer ----------

const INTERVALS = [3, 5, 10, 20]

function savedInterval(): number {
  const n = parseInt(localStorage.getItem('lp.interval') || '', 10)
  return INTERVALS.indexOf(n) >= 0 ? n : 5
}

/** Can this browser play the original video file? */
function browserPlays(d: Details, probe: HTMLVideoElement): boolean {
  const types: { [codec: string]: string } = {
    h264: 'video/mp4; codecs="avc1.640028"',
    hevc: 'video/mp4; codecs="hvc1.1.6.L120.90"',
    av1: 'video/mp4; codecs="av01.0.08M.08"',
    vp9: 'video/webm; codecs="vp9"',
    vp8: 'video/webm; codecs="vp8"',
  }
  const type = d.videoCodec ? types[d.videoCodec] : undefined
  if (!type) return false
  const mp4 = d.mime === 'video/mp4' || d.mime === 'video/quicktime'
  const containerOk = type.indexOf('webm') >= 0 ? d.mime === 'video/webm' : mp4
  const audioOk = d.audioCodec === null || ['', 'aac', 'mp3', 'opus', 'vorbis', 'flac'].indexOf(d.audioCodec) >= 0
  return containerOk && audioOk && probe.canPlayType(type) !== ''
}

async function openViewer(spec: ListSpec, id: number) {
  const items = await loadList(spec).catch(showError)
  if (!items) return
  const start = items.findIndex((i) => i.id === id)
  push(viewer(items, start < 0 ? 0 : start))
}

function viewer(items: Item[], startIndex: number): Screen {
  const touch = 'ontouchstart' in window
  let index = startIndex
  let interval = savedInterval()
  let slideshow = false
  let slideTimer = 0
  let infoTimer = 0
  let toastTimer = 0
  let barTimer = 0
  let token = 0 // bumps on every item change, so late loads are ignored

  // Video state
  let video: HTMLVideoElement | null = null
  let details: Details | null = null
  let streaming = false // converted stream: no seeking within the file
  let offset = 0 // where the converted stream started
  let duration = 0
  let stallTimer = 0

  const stage = h('div')
  const spinner = h('div', { className: 'spinner' })
  const infoName = h('div', { className: 'name' })
  const infoSub = h('div', { className: 'sub' })
  const info = h('div', { className: 'info' }, infoName, infoSub)
  const toastEl = h('div', { className: 'toast' })
  const fill = h('div', { className: 'fill' })
  const timeNow = h('span')
  const timeTotal = h('span')
  const bar = h('div', { className: 'bar' }, timeNow, h('div', { className: 'track' }, fill), timeTotal)
  const prevZone = h('div', { className: 'tap prev', onclick: () => go(-1) })
  const nextZone = h('div', { className: 'tap next', onclick: () => go(1) })
  const closeBtn = h('button', { className: 'close', onclick: back, title: 'Close' }, '×')
  const el = h('div', { className: 'viewer' }, stage, spinner, prevZone, nextZone, info, bar, toastEl, closeBtn)

  stage.addEventListener('click', () => {
    if (video && !touch) togglePlay()
    else showInfo(true)
  })

  // Swipe on phones.
  let touchX = 0
  el.addEventListener('touchstart', (e: TouchEvent) => (touchX = e.touches[0].clientX))
  el.addEventListener('touchend', (e: TouchEvent) => {
    const dx = e.changedTouches[0].clientX - touchX
    if (Math.abs(dx) > 60 && !video) go(dx < 0 ? 1 : -1)
  })

  function toast(text: string) {
    toastEl.textContent = text
    toastEl.className = 'toast show'
    clearTimeout(toastTimer)
    toastTimer = window.setTimeout(() => (toastEl.className = 'toast'), 2500)
  }

  function showInfo(flash: boolean) {
    const item = items[index]
    infoName.textContent = details && details.id === item.id ? details.name : formatDate(item.t)
    infoSub.textContent = (details && details.id === item.id ? formatDate(item.t) + ' · ' : '') + (index + 1) + ' of ' + items.length
    info.className = 'info show'
    clearTimeout(infoTimer)
    if (flash) infoTimer = window.setTimeout(() => (info.className = 'info'), 3000)
  }

  function toggleInfo() {
    if (info.className.indexOf('show') >= 0 && !infoTimer) info.className = 'info'
    else {
      showInfo(false)
      infoTimer = 0
    }
  }

  function position(): number {
    return video ? (streaming ? offset : 0) + video.currentTime : 0
  }

  function updateBar() {
    if (!video) return
    const now = position()
    timeNow.textContent = formatDuration(now)
    timeTotal.textContent = duration ? formatDuration(duration) : ''
    fill.style.width = duration ? Math.min(100, (now / duration) * 100) + '%' : '0'
  }

  function showBar() {
    if (touch) return
    updateBar()
    bar.className = 'bar show'
    clearTimeout(barTimer)
    barTimer = window.setTimeout(() => {
      if (video && !video.paused) bar.className = 'bar'
    }, 3000)
  }

  function teardown() {
    clearTimeout(slideTimer)
    clearTimeout(stallTimer)
    if (video) {
      // Dropping the source ends the download, which stops the conversion on the PC.
      video.pause()
      video.removeAttribute('src')
      try {
        video.load()
      } catch (_) {
        // ignore
      }
      video = null
    }
    details = null
    streaming = false
    offset = 0
    stage.innerHTML = ''
    bar.className = 'bar'
  }

  function error(text: string) {
    spinner.className = 'spinner'
    stage.appendChild(h('div', { className: 'error' }, text))
  }

  function show(i: number) {
    teardown()
    index = (i + items.length) % items.length
    const item = items[index]
    const mine = ++token
    spinner.className = 'spinner show'
    prevZone.style.display = nextZone.style.display = item.v && touch ? 'none' : ''
    if (item.v) showVideo(item, mine)
    else showPhoto(item, mine)
    showInfo(true)
  }

  function showPhoto(item: Item, mine: number) {
    const img = new Image()
    img.className = 'photo'
    img.onload = () => {
      if (mine !== token) return
      spinner.className = 'spinner'
      stage.innerHTML = ''
      stage.appendChild(img)
      preload()
      if (slideshow) schedule()
    }
    img.onerror = () => {
      if (mine !== token) return
      error("This photo can't be shown.")
      if (slideshow) schedule()
    }
    img.src = fileUrl(item.id, 'photo.jpg', item.m)
  }

  /** Load the neighbours' photos so left/right is instant. */
  function preload() {
    for (const d of [1, -1]) {
      const n = items[(index + d + items.length) % items.length]
      if (!n.v) new Image().src = fileUrl(n.id, 'photo.jpg', n.m)
    }
  }

  async function showVideo(item: Item, mine: number) {
    let d: Details
    try {
      d = await get<Details>('/media/' + item.id)
    } catch (e) {
      if (mine === token) error("This video can't be played.")
      return
    }
    if (mine !== token) return
    details = d
    duration = (d.durationMs || item.d || 0) / 1000
    const v = h('video', { autoplay: true, playsInline: true, controls: touch, preload: 'auto' }) as HTMLVideoElement
    v.setAttribute('playsinline', '')
    video = v
    v.addEventListener('playing', () => {
      clearTimeout(stallTimer)
      spinner.className = 'spinner'
      showBar()
    })
    v.addEventListener('waiting', () => (spinner.className = 'spinner show'))
    v.addEventListener('timeupdate', () => {
      if (bar.className.indexOf('show') >= 0) updateBar()
    })
    v.addEventListener('pause', () => showBar())
    v.addEventListener('ended', () => {
      if (slideshow) go(1)
      else showBar()
    })
    v.addEventListener('error', () => {
      if (v !== video) return
      // The original didn't play after all: fall back to the converted stream.
      if (!streaming && d.canConvert) startStream(position())
      else error(d.canConvert ? "This video can't be played." : "This TV can't play this video. Turn on video conversion in Memora settings.")
    })
    stage.appendChild(v)
    if (browserPlays(d, v) || !d.canConvert) {
      v.src = fileUrl(item.id, 'original')
      // Some browsers say they can play a codec, then never show a frame (no error either).
      if (d.canConvert) {
        stallTimer = window.setTimeout(() => {
          if (v === video && !streaming && v.readyState < 2) startStream(v.currentTime)
        }, 8000)
      }
    } else {
      startStream(0)
    }
    showInfo(true)
  }

  function startStream(at: number) {
    if (!video || !details) return
    streaming = true
    offset = Math.max(0, at)
    spinner.className = 'spinner show'
    video.src = fileUrl(details.id, 'stream.mp4') + '?t=' + offset.toFixed(1)
    const p = video.play() as Promise<void> | undefined
    if (p && p.catch) p.catch(() => undefined)
  }

  function seek(delta: number) {
    if (!video) return
    const target = Math.max(0, Math.min(duration ? duration - 1 : Infinity, position() + delta))
    if (streaming) startStream(target)
    else video.currentTime = target
    showBar()
  }

  function togglePlay() {
    if (!video) return
    if (video.paused) {
      const p = video.play() as Promise<void> | undefined
      if (p && p.catch) p.catch(() => undefined)
    } else video.pause()
    showBar()
  }

  function schedule() {
    clearTimeout(slideTimer)
    slideTimer = window.setTimeout(() => {
      // Skip videos in a slideshow.
      for (let step = 1; step <= items.length; step++) {
        const n = (index + step) % items.length
        if (!items[n].v) return show(n)
      }
    }, interval * 1000)
  }

  function go(delta: number) {
    show(index + delta)
  }

  function setSlideshow(on: boolean) {
    slideshow = on
    if (on) {
      toast('Slideshow: every ' + interval + ' seconds. Press OK to stop.')
      schedule()
    } else {
      clearTimeout(slideTimer)
      toast('Slideshow stopped')
    }
  }

  function onKey(key: Key): boolean {
    const isVideo = !!video
    switch (key) {
      case 'left':
        if (isVideo) seek(-10)
        else go(-1)
        return true
      case 'right':
        if (isVideo) seek(10)
        else go(1)
        return true
      case 'rw':
        seek(-30)
        return true
      case 'ff':
        seek(30)
        return true
      case 'next':
        go(1)
        return true
      case 'prev':
        go(-1)
        return true
      case 'enter':
        if (isVideo) togglePlay()
        else setSlideshow(!slideshow)
        return true
      case 'playpause':
      case 'play':
      case 'pause':
        if (isVideo) togglePlay()
        else setSlideshow(key === 'pause' ? false : key === 'play' ? true : !slideshow)
        return true
      case 'up':
        toggleInfo()
        return true
      case 'down':
        if (isVideo) showBar()
        else {
          interval = INTERVALS[(INTERVALS.indexOf(interval) + 1) % INTERVALS.length]
          localStorage.setItem('lp.interval', String(interval))
          toast('Slideshow speed: every ' + interval + ' seconds')
          if (slideshow) schedule()
        }
        return true
    }
    return false
  }

  show(startIndex)
  setTimeout(() => {
    if (!touch) toast(items[index].v ? '◀ ▶ seek · OK play/pause · CH+/CH− next' : '◀ ▶ browse · OK slideshow · ▼ speed · ▲ info')
  }, 600)

  return {
    el,
    onKey,
    onClose: () => {
      token++
      teardown()
      clearTimeout(infoTimer)
      clearTimeout(toastTimer)
      clearTimeout(barTimer)
    },
  }
}

// ---------- PIN ----------

function pinScreen(name: string, done: () => void): Screen {
  let digits = ''
  const dots = h('div', { className: 'dots' })
  const err = h('div', { className: 'error' })
  const render = () => {
    let s = ''
    for (let i = 0; i < digits.length; i++) s += '●'
    dots.textContent = s || ' '
  }
  const add = (d: string) => {
    if (digits.length < 8) digits += d
    err.textContent = ''
    render()
  }
  const del = () => {
    digits = digits.slice(0, -1)
    render()
  }
  const submit = async () => {
    if (digits.length < 4) {
      err.textContent = 'The PIN has 4 to 8 digits.'
      return
    }
    try {
      await call<void>('POST', '/login', { pin: digits })
      done()
    } catch (e) {
      err.textContent = e instanceof Error ? e.message : String(e)
      digits = ''
      render()
    }
  }
  const key = (label: string, action: () => void) => h('button', { className: 'key focusable', onclick: action }, label)
  const keys = h('div', { className: 'keys' })
  for (const d of ['1', '2', '3', '4', '5', '6', '7', '8', '9']) keys.appendChild(key(d, () => add(d)))
  keys.appendChild(key('⌫', del))
  keys.appendChild(key('0', () => add('0')))
  const ok = key('OK', submit)
  keys.appendChild(ok)
  render()
  const el = h('div', { className: 'pin' }, h('h1', null, name), h('p', null, 'Enter the PIN set in Memora on the PC.'), dots, err, keys)
  return {
    el,
    onKey: (k, e) => {
      const code = e.keyCode
      if ((code >= 48 && code <= 57) || (code >= 96 && code <= 105)) {
        add(String(code >= 96 ? code - 96 : code - 48))
        // Typing on a keyboard or a remote's number keys: Enter should then submit.
        focusEl(ok)
        return true
      }
      if (k === 'delete') {
        del()
        return true
      }
      return false
    },
  }
}

// ---------- start ----------

function showError(e: unknown): undefined {
  const message = e instanceof Error ? e.message : String(e)
  setRoot({
    el: h(
      'div',
      { className: 'message' },
      h('p', null, 'Could not reach Memora on the PC.'),
      h('p', null, message),
      h('button', { className: 'btn focusable', onclick: () => location.reload() }, 'Try again'),
    ),
  })
  return undefined
}

async function start() {
  registerTvKeys()
  try {
    const hello = await get<Hello>('/hello')
    document.title = hello.name
    const openHome = async () => setRoot(await homeScreen())
    if (hello.pinRequired && !hello.authorized) setRoot(pinScreen(hello.name, () => openHome().catch(showError)))
    else await openHome()
  } catch (e) {
    showError(e)
  }
}

start()
