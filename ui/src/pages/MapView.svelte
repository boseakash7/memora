<script lang="ts">
  import { onMount, untrack } from 'svelte'
  import 'maplibre-gl/dist/maplibre-gl.css'
  // MapLibre looks for its worker file next to its own module, which is not where a bundler
  // puts it, so point it there explicitly. The file is self-contained.
  import workerUrl from 'maplibre-gl/dist/maplibre-gl-worker.mjs?url'
  import type { GeoJSONSource, Map as MapLibreMap, Marker } from 'maplibre-gl'
  import MediaPage from '../components/MediaPage.svelte'
  import { api, errorMessage, type MapPoint } from '../lib/api'
  import { buildStyle, mapColors } from '../lib/mapStyle'
  import Icon from '../lib/Icon.svelte'
  import { library } from '../lib/library.svelte'

  let container: HTMLDivElement
  /** Area shown on the map, as `west,south,east,north`. The grid below lists the photos in it. */
  let bbox = $state<string | null>(null)
  let pointCount = $state<number | null>(null)
  let error = $state('')

  let map: MapLibreMap | undefined
  let loadPoints: (() => Promise<void>) | undefined

  onMount(() => {
    let disposed = false
    const markers = new Map<number, Marker>()

    ;(async () => {
      // Loaded on demand: the map code is the heaviest part of the app.
      const [maplibregl, countries] = await Promise.all([
        import('maplibre-gl'),
        fetch(new URL('map/countries.json', document.baseURI)).then((r) => r.json()),
      ])
      if (disposed) return
      maplibregl.setWorkerUrl(new URL(workerUrl, location.href).href)

      const colors = mapColors()

      // No tile server: country shapes, borders, rivers, lakes and city names bundled with the
      // app, so everything works offline.
      const m = new maplibregl.Map({
        container,
        center: [10, 25],
        zoom: 1.4,
        minZoom: 0.6,
        maxZoom: 11, // the bundled data is good to about here
        attributionControl: false,
        style: buildStyle(countries, colors),
      })
      map = m
      m.addControl(new maplibregl.NavigationControl({ showCompass: false }), 'top-right')

      const wrap = (lon: number) => ((((lon + 180) % 360) + 360) % 360) - 180
      const updateBox = () => {
        const b = m.getBounds()
        const south = Math.max(-90, b.getSouth())
        const north = Math.min(90, b.getNorth())
        const [west, east] = b.getEast() - b.getWest() >= 360 ? [-180, 180] : [wrap(b.getWest()), wrap(b.getEast())]
        bbox = `${west.toFixed(5)},${south.toFixed(5)},${east.toFixed(5)},${north.toFixed(5)}`
      }

      const clusterLabel = (n: number) => (n >= 10000 ? `${Math.round(n / 1000)}k` : n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n))

      // Clusters are drawn as HTML markers because the offline map style has no fonts for text layers.
      const updateClusters = () => {
        if (!m.getSource('photos') || !m.isSourceLoaded('photos')) return
        const source = m.getSource('photos') as GeoJSONSource
        const seen = new Set<number>()
        for (const f of m.querySourceFeatures('photos', { filter: ['has', 'point_count'] })) {
          const id = f.properties.cluster_id as number
          if (seen.has(id)) continue
          seen.add(id)
          if (markers.has(id)) continue
          const count = f.properties.point_count as number
          const [lon, lat] = (f.geometry as unknown as { coordinates: [number, number] }).coordinates
          const el = document.createElement('button')
          el.className = 'map-cluster'
          el.textContent = clusterLabel(count)
          const size = Math.min(64, 30 + Math.log10(count) * 12)
          el.style.width = el.style.height = `${size}px`
          el.title = `${count.toLocaleString()} photos and videos`
          el.onclick = async () => {
            const zoom = await source.getClusterExpansionZoom(id)
            m.easeTo({ center: [lon, lat], zoom })
          }
          markers.set(id, new maplibregl.Marker({ element: el }).setLngLat([lon, lat]).addTo(m))
        }
        for (const [id, marker] of markers) {
          if (!seen.has(id)) {
            marker.remove()
            markers.delete(id)
          }
        }
      }

      let fitted = false
      loadPoints = async () => {
        try {
          const points: MapPoint[] = await api.mapPoints()
          if (disposed) return
          pointCount = points.length
          for (const marker of markers.values()) marker.remove()
          markers.clear()
          const data = {
            type: 'FeatureCollection' as const,
            features: points.map(([id, lat, lon]) => ({
              type: 'Feature' as const,
              properties: { id },
              geometry: { type: 'Point' as const, coordinates: [lon, lat] },
            })),
          }
          const source = m.getSource('photos') as GeoJSONSource | undefined
          if (source) source.setData(data)
          else {
            m.addSource('photos', { type: 'geojson', data, cluster: true, clusterRadius: 55, clusterMaxZoom: 13 })
            m.addLayer({
              id: 'single',
              type: 'circle',
              source: 'photos',
              filter: ['!', ['has', 'point_count']],
              paint: {
                'circle-radius': 7,
                'circle-color': colors.dot,
                'circle-stroke-width': 2,
                'circle-stroke-color': '#ffffff',
              },
            })
          }
          if (!fitted && points.length > 0) {
            fitted = true
            const b = new maplibregl.LngLatBounds()
            for (const [, lat, lon] of points) b.extend([lon, lat])
            m.fitBounds(b, { padding: 70, maxZoom: 9, duration: 0 })
          }
        } catch (e) {
          error = errorMessage(e)
        }
      }

      m.on('load', async () => {
        await loadPoints?.()
        updateBox()
      })
      m.on('moveend', updateBox)
      m.on('move', updateClusters)
      // Once everything has settled, so markers of clusters that no longer exist are removed.
      m.on('idle', updateClusters)
      m.on('data', (e) => {
        if ('sourceId' in e && e.sourceId === 'photos') updateClusters()
      })
      m.on('click', 'single', (e) => {
        const f = e.features?.[0]
        if (!f) return
        const [lon, lat] = (f.geometry as unknown as { coordinates: [number, number] }).coordinates
        m.easeTo({ center: [lon, lat], zoom: Math.min(Math.max(m.getZoom() + 3, 9), 11) })
      })
      m.on('mouseenter', 'single', () => (m.getCanvas().style.cursor = 'pointer'))
      m.on('mouseleave', 'single', () => (m.getCanvas().style.cursor = ''))
    })().catch((e) => (error = errorMessage(e)))

    return () => {
      disposed = true
      map?.remove()
      map = undefined
    }
  })

  // Pick up photos that finish analysis while the map is open.
  let seen: number | null = null
  let timer: ReturnType<typeof setTimeout> | undefined
  $effect(() => {
    const revision = library.status?.revision ?? null
    untrack(() => {
      if (seen !== null && revision !== seen) {
        clearTimeout(timer)
        timer = setTimeout(() => loadPoints?.(), 1000)
      }
      seen = revision
    })
  })
  $effect(() => () => clearTimeout(timer))
</script>

<div class="map-page">
  <div class="map-canvas">
    <div class="map-container" bind:this={container}></div>
    {#if error}
      <p class="error map-note">{error}</p>
    {:else if pointCount !== null}
      <div class="map-note">
        {#if pointCount === 0}
          No photos with a location yet. Photos with GPS data appear here.
        {:else}
          {pointCount.toLocaleString()} {pointCount === 1 ? 'photo or video' : 'photos and videos'} with a location
        {/if}
      </div>
    {/if}
  </div>
  <div class="map-grid">
    {#if bbox}
      <MediaPage query={{ bbox }}>
        {#snippet empty()}
          <div class="empty small">
            <p>No photos in this part of the map. Move or zoom out.</p>
          </div>
        {/snippet}
      </MediaPage>
    {/if}
  </div>
</div>
