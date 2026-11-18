/** Minimal hash router: `#/photos`, `#/album/3`, `#/search?q=beach`, ... */

export const ROUTES = [
  'photos',
  'memories',
  'explore',
  'favorites',
  'archive',
  'albums',
  'album',
  'people',
  'person',
  'places',
  'place',
  'map',
  'utilities',
  'duplicates',
  'vault',
  'trash',
  'search',
  'settings',
] as const
export type Route = (typeof ROUTES)[number]

type Parsed = {
  route: Route
  /** Id on `#/album/<id>`, `#/place/<id>` and `#/person/<id>`. */
  id: number | null
  params: URLSearchParams
}

function parse(): Parsed {
  const [path, query = ''] = location.hash.replace(/^#\/?/, '').split('?')
  const [name, arg] = path.split('/')
  const route = (ROUTES as readonly string[]).includes(name) ? (name as Route) : 'photos'
  const id = arg !== undefined && /^\d+$/.test(arg) ? Number(arg) : null
  // Detail routes need an id; without one, fall back to their list.
  const detail: Partial<Record<Route, Route>> = { album: 'albums', place: 'places', person: 'people' }
  return { route: id === null && detail[route] ? detail[route] : route, id, params: new URLSearchParams(query) }
}

export const router = $state(parse())

window.addEventListener('hashchange', () => {
  Object.assign(router, parse())
})

export function go(route: Route) {
  location.hash = `/${route}`
}

export function goAlbum(id: number) {
  location.hash = `/album/${id}`
}

export function goPlace(id: number) {
  location.hash = `/place/${id}`
}

export function goPerson(id: number) {
  location.hash = `/person/${id}`
}

/**
 * Open the search page; blank values are left out. With `replace` the current history entry
 * is updated instead, so Back doesn't step through every filter change.
 */
export function goSearch(params: Record<string, string | undefined>, replace = false) {
  const q = new URLSearchParams()
  for (const [k, v] of Object.entries(params)) if (v) q.set(k, v)
  const hash = `#/search${q.size ? `?${q}` : ''}`
  if (replace) {
    history.replaceState(null, '', hash)
    Object.assign(router, parse())
  } else {
    location.hash = hash
  }
}
