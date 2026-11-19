import { api, errorMessage, type Feature, type FeatureStatus } from './api'
import type { IconName } from './icons'
import { toast } from './toast.svelte'

/** What each AI feature does, in plain words, for the welcome screen and Settings. */
export const FEATURE_INFO: Record<Feature, { title: string; text: string; short: string; icon: IconName; tint: string }> = {
  faces: {
    title: 'People and faces',
    text: 'Finds faces in your photos and groups them by person, so you can see every photo of someone in one place.',
    short: 'Groups your photos by the people in them.',
    icon: 'people',
    tint: 'blue',
  },
  search: {
    title: 'Search by what’s in a photo',
    text: 'Type things like “beach”, “dog” or “birthday cake” to find photos. Also suggests a filter that suits each photo in the editor.',
    short: 'Find photos by typing what is in them; filter suggestions in the editor.',
    icon: 'search',
    tint: 'amber',
  },
  blur: {
    title: 'Background blur',
    text: 'Blurs the background behind the main subject in the photo editor, like portrait mode on a phone.',
    short: 'Blur behind the subject in the editor.',
    icon: 'portrait',
    tint: 'green',
  },
}

export const FEATURES: Feature[] = ['faces', 'search', 'blur']

/** Whole megabytes, the way download sizes are shown. */
export const mb = (bytes: number) => `${Math.max(1, Math.round(bytes / 1048576))} MB`

/** The features' models, kept up to date while anything shows them. */
export const features = $state<{ list: FeatureStatus[] | null; error: string }>({ list: null, error: '' })

let watchers = 0
let timer: ReturnType<typeof setTimeout> | undefined
/** Features that were downloading at the last look, to tell when one finishes. */
let wasBusy = new Set<Feature>()

const busy = (f: FeatureStatus) => f.job?.state === 'downloading' || f.job?.state === 'queued'

export function feature(f: Feature): FeatureStatus | undefined {
  return features.list?.find((s) => s.feature === f)
}

function apply(list: FeatureStatus[]) {
  for (const f of list) {
    if (wasBusy.has(f.feature) && f.installed) toast(`${FEATURE_INFO[f.feature].title} is ready`)
    if (wasBusy.has(f.feature) && f.job?.state === 'failed') toast(`${FEATURE_INFO[f.feature].title}: ${f.job.error}`, { error: true })
  }
  wasBusy = new Set(list.filter(busy).map((f) => f.feature))
  features.list = list
  features.error = ''
  schedule()
}

function schedule() {
  clearTimeout(timer)
  if (watchers === 0) return
  // Quick while something downloads, slow otherwise (models copied in by hand show up too).
  timer = setTimeout(load, wasBusy.size > 0 ? 500 : 5000)
}

function load() {
  api.ai
    .features()
    .then(apply)
    .catch((e) => {
      features.error = errorMessage(e)
      schedule()
    })
}

/** Keep `features` fresh while the calling component is mounted (use in an $effect). */
export function watchFeatures(): () => void {
  watchers++
  if (watchers === 1) load()
  return () => {
    watchers--
    if (watchers === 0) clearTimeout(timer)
  }
}

async function act(call: Promise<FeatureStatus[]>) {
  try {
    apply(await call)
  } catch (e) {
    toast(errorMessage(e), { error: true })
  }
}

export const downloadFeature = (f: Feature) => act(api.ai.download(f))
export const pauseFeature = (f: Feature) => act(api.ai.pause(f))
export const cancelFeature = (f: Feature) => act(api.ai.cancel(f))
export const removeFeature = (f: Feature) => act(api.ai.remove(f))
