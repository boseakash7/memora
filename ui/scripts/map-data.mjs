// Builds the offline map data in public/map/ from Natural Earth (public domain) and
// Protomaps' Noto Sans glyphs (SIL OFL). Run once, or when the data should be refreshed:
//
//   npm run map-data
//
// The app never downloads anything: the results are committed and shipped with it.
// Needs internet while this script runs.
import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

const NE = 'https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson'
const GLYPHS = 'https://raw.githubusercontent.com/protomaps/basemaps-assets/main/fonts'
const out = path.resolve(import.meta.dirname, '../public/map')
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'map-data-'))
fs.mkdirSync(out, { recursive: true })

async function download(url, file) {
  const res = await fetch(url)
  if (!res.ok) throw new Error(`${url}: ${res.status}`)
  fs.writeFileSync(file, Buffer.from(await res.arrayBuffer()))
}

/** Keep only the properties the map style uses, so the files stay small. */
function slim(file, keep) {
  const json = JSON.parse(fs.readFileSync(file, 'utf8'))
  for (const f of json.features) {
    const props = {}
    for (const [to, from] of Object.entries(keep)) {
      const v = typeof from === 'function' ? from(f.properties) : f.properties[from]
      if (v !== null && v !== undefined) props[to] = v
    }
    f.properties = props
  }
  fs.writeFileSync(file, JSON.stringify(json))
}

function simplify(file, target, percent) {
  // Run mapshaper's script with node directly: no shell, so paths with spaces are fine.
  const bin = path.resolve(import.meta.dirname, '../node_modules/mapshaper/bin/mapshaper')
  execFileSync(process.execPath, [bin, file, '-simplify', `${percent}%`, 'keep-shapes', '-o', target, 'precision=0.01', 'format=geojson'], {
    stdio: 'inherit',
  })
}

const layers = {
  // Country shapes. Natural Earth's own polygons are cut at the 180th meridian, which flat maps need
  // (the `world-atlas` package is built for spherical projections and breaks Russia, Fiji, Antarctica).
  countries: { source: 'ne_50m_admin_0_countries', keep: { name: 'NAME' }, percent: 35 },
  // Towns and cities, with the zoom level Natural Earth recommends showing them from.
  places: {
    source: 'ne_10m_populated_places_simple',
    keep: { n: 'name', z: 'min_zoom', p: 'pop_max', c: (p) => (p.adm0cap === 1 ? 1 : undefined) },
    percent: 100,
  },
  // State / province borders.
  states: { source: 'ne_10m_admin_1_states_provinces_lines', keep: { z: 'MIN_ZOOM' }, percent: 6 },
  lakes: { source: 'ne_10m_lakes', keep: { z: 'min_zoom' }, percent: 12 },
  rivers: { source: 'ne_10m_rivers_lake_centerlines', keep: { z: 'min_zoom' }, percent: 12 },
}

for (const [name, layer] of Object.entries(layers)) {
  console.log(`\n== ${name}`)
  const raw = path.join(tmp, `${layer.source}.geojson`)
  await download(`${NE}/${layer.source}.geojson`, raw)
  slim(raw, layer.keep)
  const target = path.join(out, `${name}.json`)
  if (layer.percent < 100) simplify(raw, target, layer.percent)
  else {
    // Points: just round the coordinates.
    const json = JSON.parse(fs.readFileSync(raw, 'utf8'))
    for (const f of json.features) f.geometry.coordinates = f.geometry.coordinates.map((c) => Math.round(c * 100) / 100)
    fs.writeFileSync(target, JSON.stringify(json))
  }
  console.log(`${name}.json: ${(fs.statSync(target).size / 1024).toFixed(0)} KB`)
}

// Glyphs for map labels (Latin letters only: the place names are in English).
for (const font of ['Noto Sans Regular', 'Noto Sans Medium']) {
  const dir = path.join(out, 'fonts', font)
  fs.mkdirSync(dir, { recursive: true })
  for (const range of ['0-255', '256-511']) {
    await download(`${GLYPHS}/${encodeURIComponent(font)}/${range}.pbf`, path.join(dir, `${range}.pbf`))
  }
}
console.log('\nglyphs done')
fs.rmSync(tmp, { recursive: true, force: true })
