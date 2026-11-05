import type { FilterSpecification, LayerSpecification, StyleSpecification } from 'maplibre-gl'

/** Colors of the map, read from CSS variables so it follows the light/dark theme. */
export type MapColors = {
  ocean: string
  land: string
  water: string
  border: string
  state: string
  label: string
  halo: string
  /** Photos that are not part of a cluster. */
  dot: string
}

export function mapColors(): MapColors {
  const css = getComputedStyle(document.documentElement)
  const get = (name: string) => css.getPropertyValue(name).trim()
  return {
    ocean: get('--map-ocean'),
    land: get('--map-land'),
    water: get('--map-water'),
    border: get('--map-border'),
    state: get('--map-state'),
    label: get('--map-label'),
    halo: get('--map-halo'),
    dot: get('--map-dot'),
  }
}

/**
 * Zoom levels at which more detail appears. Natural Earth gives every feature the zoom it is
 * meant to be shown from (`z`); a layer per band switches those features on at the right time.
 */
const BANDS = [0, 2, 3, 4, 5, 6, 7, 8]

/** Data files in public/map/, made by `npm run map-data`. */
const dataUrl = (name: string) => new URL(`map/${name}.json`, document.baseURI).href

/** One layer per zoom band: features with `z` in [from, next) show from zoom `from`. */
function banded(
  id: string,
  make: (id: string, filter: FilterSpecification, minzoom: number) => LayerSpecification,
): LayerSpecification[] {
  return BANDS.map((from, i) => {
    const to = BANDS[i + 1]
    const inBand: FilterSpecification =
      to === undefined
        ? ['>=', ['coalesce', ['get', 'z'], 0], from]
        : ['all', ['>=', ['coalesce', ['get', 'z'], 0], i === 0 ? -1 : from], ['<', ['coalesce', ['get', 'z'], 0], to]]
    return make(`${id}-${from}`, inBand, from)
  })
}

type Ring = number[][]

/** Area and centroid of a ring in degrees (good enough to place a label). */
function ringStats(ring: Ring): { area: number; x: number; y: number } {
  let a = 0
  let cx = 0
  let cy = 0
  for (let i = 0; i < ring.length - 1; i++) {
    const [x0, y0] = ring[i]
    const [x1, y1] = ring[i + 1]
    const cross = x0 * y1 - x1 * y0
    a += cross
    cx += (x0 + x1) * cross
    cy += (y0 + y1) * cross
  }
  if (Math.abs(a) < 1e-9) {
    const xs = ring.map((p) => p[0])
    const ys = ring.map((p) => p[1])
    return { area: 0, x: (Math.min(...xs) + Math.max(...xs)) / 2, y: (Math.min(...ys) + Math.max(...ys)) / 2 }
  }
  return { area: Math.abs(a) / 2, x: cx / (3 * a), y: cy / (3 * a) }
}

/**
 * One label point per country, in its largest landmass. Labelling the polygons themselves
 * would repeat the name for every map tile a country is cut into.
 */
function countryLabelPoints(countries: { features: { properties: { name?: string }; geometry: { type: string; coordinates: unknown } }[] }) {
  const features = countries.features.flatMap((f) => {
    const polygons = (f.geometry.type === 'Polygon' ? [f.geometry.coordinates] : f.geometry.coordinates) as Ring[][]
    const biggest = polygons.map((p) => ringStats(p[0])).sort((a, b) => b.area - a.area)[0]
    if (!biggest || !f.properties.name) return []
    return [
      {
        type: 'Feature' as const,
        properties: { name: f.properties.name, area: biggest.area },
        geometry: { type: 'Point' as const, coordinates: [biggest.x, biggest.y] },
      },
    ]
  })
  return { type: 'FeatureCollection' as const, features }
}

/** An offline basemap: countries, states, lakes, rivers, and labelled cities. No tiles, no network. */
export function buildStyle(countries: unknown, c: MapColors): StyleSpecification {
  const text = (font: 'Regular' | 'Medium') => [`Noto Sans ${font}`]
  const labelPaint = { 'text-color': c.label, 'text-halo-color': c.halo, 'text-halo-width': 1.4 }

  const layers: LayerSpecification[] = [
    { id: 'ocean', type: 'background', paint: { 'background-color': c.ocean } },
    { id: 'land', type: 'fill', source: 'countries', paint: { 'fill-color': c.land } },
    ...banded('lakes', (id, filter, minzoom) => ({
      id,
      type: 'fill',
      source: 'lakes',
      minzoom,
      filter,
      paint: { 'fill-color': c.ocean },
    })),
    ...banded('rivers', (id, filter, minzoom) => ({
      id,
      type: 'line',
      source: 'rivers',
      minzoom,
      filter,
      paint: {
        'line-color': c.water,
        'line-width': ['interpolate', ['linear'], ['zoom'], 2, 0.4, 8, 1.4],
      },
    })),
    ...banded('states', (id, filter, minzoom) => ({
      id,
      type: 'line',
      source: 'states',
      minzoom,
      filter,
      paint: {
        'line-color': c.state,
        'line-width': ['interpolate', ['linear'], ['zoom'], 2, 0.4, 8, 1.1],
        'line-dasharray': [3, 2],
      },
    })),
    {
      id: 'borders',
      type: 'line',
      source: 'countries',
      paint: { 'line-color': c.border, 'line-width': ['interpolate', ['linear'], ['zoom'], 1, 0.6, 8, 1.6] },
    },
    {
      id: 'country-labels',
      type: 'symbol',
      source: 'country-points',
      maxzoom: 7,
      layout: {
        'text-field': ['get', 'name'],
        'text-font': text('Medium'),
        'text-size': ['interpolate', ['linear'], ['zoom'], 1, 10, 6, 17],
        'text-transform': 'uppercase',
        'text-letter-spacing': 0.12,
        'text-max-width': 7,
        'symbol-sort-key': ['-', 0, ['get', 'area']], // big countries first
      },
      paint: { ...labelPaint, 'text-opacity': 0.75 },
    },
    // Cities: a dot and a name, from the zoom Natural Earth recommends. MapLibre drops labels
    // that would collide, biggest cities first.
    ...banded('city-dot', (id, filter, minzoom) => ({
      id,
      type: 'circle',
      source: 'places',
      minzoom,
      filter,
      paint: {
        'circle-radius': ['case', ['==', ['get', 'c'], 1], 3.4, 2.4],
        'circle-color': c.label,
        'circle-stroke-color': c.halo,
        'circle-stroke-width': 1,
      },
    })),
    ...banded('city-label', (id, filter, minzoom) => ({
      id,
      type: 'symbol',
      source: 'places',
      minzoom,
      filter,
      layout: {
        'text-field': ['get', 'n'],
        'text-font': ['case', ['==', ['get', 'c'], 1], ['literal', text('Medium')], ['literal', text('Regular')]],
        'text-size': ['interpolate', ['linear'], ['zoom'], 3, 10, 9, 14],
        'text-anchor': 'top',
        'text-offset': [0, 0.5],
        'text-max-width': 8,
        'symbol-sort-key': ['-', 0, ['coalesce', ['get', 'p'], 0]],
      },
      paint: labelPaint,
    })),
  ]

  return {
    version: 8,
    // Latin glyphs bundled in public/map/fonts; `{fontstack}` and `{range}` are filled in by MapLibre.
    glyphs: `${new URL('map/fonts/', document.baseURI).href}{fontstack}/{range}.pbf`,
    sources: {
      countries: { type: 'geojson', data: countries as never },
      'country-points': { type: 'geojson', data: countryLabelPoints(countries as never) },
      states: { type: 'geojson', data: dataUrl('states') },
      lakes: { type: 'geojson', data: dataUrl('lakes') },
      rivers: { type: 'geojson', data: dataUrl('rivers') },
      places: { type: 'geojson', data: dataUrl('places') },
    },
    layers,
  }
}
