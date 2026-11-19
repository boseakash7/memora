/**
 * Photo edits as the editor works with them. The same recipe is stored by the PC and
 * applied whenever a picture of the photo is made (see `photos_core::edit`).
 */
import { ApiError, serverInfo } from './api'

export type Crop = { x: number; y: number; w: number; h: number }

export type Adjust = {
  exposure: number
  contrast: number
  highlights: number
  shadows: number
  whites: number
  blacks: number
  temperature: number
  tint: number
  saturation: number
  vibrance: number
  clarity: number
  dehaze: number
  sharpen: number
  vignette: number
  grain: number
}

export type Edit = {
  /** Quarter turns clockwise. */
  rotate: number
  /** Mirrored left-right (before the turns). */
  flip: boolean
  /** Degrees clockwise, -45..45. */
  straighten: number
  /** Fractions of the turned picture. */
  crop: Crop | null
  adjust: Adjust
  filter: string | null
  /** 0..1 */
  filterStrength: number
  /** Background blur, 0..100. */
  blur: number
  blurStyle: BlurStyle
}

export type BlurStyle = 'lens' | 'oval' | 'soft' | 'swirl' | 'motion' | 'zoom'

export const BLUR_STYLES: { id: BlurStyle; label: string; about: string }[] = [
  { id: 'lens', label: 'Lens', about: 'Round bokeh like a fast lens; bright lights become soft discs' },
  { id: 'oval', label: 'Oval', about: 'Tall oval bokeh, like an anamorphic cinema lens' },
  { id: 'soft', label: 'Soft', about: 'A smooth, even blur' },
  { id: 'swirl', label: 'Swirl', about: 'Swirls around the centre, like old Petzval and Helios lenses' },
  { id: 'motion', label: 'Motion', about: 'Sideways streaks, as when panning with a moving subject' },
  { id: 'zoom', label: 'Zoom', about: 'Streaks out from the subject, as when zooming during the shot' },
]

export type FilterInfo = {
  id: string
  name: string
  group: 'everyday' | 'film' | 'cinematic' | 'vintage' | 'mono'
  about: string
}

export type Scene =
  | 'portrait'
  | 'landscape'
  | 'food'
  | 'night'
  | 'city'
  | 'document'
  | 'sunset'
  | 'pet'
  | 'flowers'
  | 'snow'
  | 'other'

export const GROUPS: { id: FilterInfo['group']; label: string }[] = [
  { id: 'everyday', label: 'Everyday' },
  { id: 'film', label: 'Film' },
  { id: 'cinematic', label: 'Cinematic' },
  { id: 'vintage', label: 'Vintage' },
  { id: 'mono', label: 'Black & white' },
]

export const SCENE_LABEL: Record<Scene, string> = {
  portrait: 'people',
  landscape: 'a landscape',
  food: 'food',
  night: 'a night scene',
  city: 'a city scene',
  document: 'a document',
  sunset: 'a sunset',
  pet: 'an animal',
  flowers: 'flowers',
  snow: 'a snowy scene',
  other: 'this photo',
}

export function zeroAdjust(): Adjust {
  return {
    exposure: 0,
    contrast: 0,
    highlights: 0,
    shadows: 0,
    whites: 0,
    blacks: 0,
    temperature: 0,
    tint: 0,
    saturation: 0,
    vibrance: 0,
    clarity: 0,
    dehaze: 0,
    sharpen: 0,
    vignette: 0,
    grain: 0,
  }
}

export function blankEdit(): Edit {
  return {
    rotate: 0,
    flip: false,
    straighten: 0,
    crop: null,
    adjust: zeroAdjust(),
    filter: null,
    filterStrength: 1,
    blur: 0,
    blurStyle: 'lens',
  }
}

/** A stored recipe with every field present (older or partial recipes fill in defaults). */
export function complete(e: Partial<Edit> | null | undefined): Edit {
  const b = blankEdit()
  if (!e) return b
  return { ...b, ...e, adjust: { ...b.adjust, ...(e.adjust ?? {}) } }
}

export function sameEdit(a: Edit, b: Edit): boolean {
  return JSON.stringify(a) === JSON.stringify(b)
}

export function isBlank(e: Edit): boolean {
  return sameEdit(complete(e), blankEdit()) || sameEdit({ ...e, filterStrength: 1 }, blankEdit())
}

export type Slider = { key: keyof Adjust; label: string; min: number; max: number }

export const SLIDERS: { title: string; items: Slider[] }[] = [
  {
    title: 'Light',
    items: [
      { key: 'exposure', label: 'Exposure', min: -100, max: 100 },
      { key: 'contrast', label: 'Contrast', min: -100, max: 100 },
      { key: 'highlights', label: 'Highlights', min: -100, max: 100 },
      { key: 'shadows', label: 'Shadows', min: -100, max: 100 },
      { key: 'whites', label: 'Whites', min: -100, max: 100 },
      { key: 'blacks', label: 'Blacks', min: -100, max: 100 },
    ],
  },
  {
    title: 'Colour',
    items: [
      { key: 'temperature', label: 'Warmth', min: -100, max: 100 },
      { key: 'tint', label: 'Tint', min: -100, max: 100 },
      { key: 'vibrance', label: 'Vibrance', min: -100, max: 100 },
      { key: 'saturation', label: 'Saturation', min: -100, max: 100 },
    ],
  },
  {
    title: 'Detail and effects',
    items: [
      { key: 'clarity', label: 'Clarity', min: -100, max: 100 },
      { key: 'dehaze', label: 'Dehaze', min: -100, max: 100 },
      { key: 'sharpen', label: 'Sharpen', min: 0, max: 100 },
      { key: 'vignette', label: 'Vignette', min: -100, max: 100 },
      { key: 'grain', label: 'Grain', min: 0, max: 100 },
    ],
  },
]

// ---------- crop geometry ----------

/** Aspect presets: width / height of the crop in pixels; null is free, 0 the photo's own. */
export const ASPECTS: { label: string; ratio: number | null }[] = [
  { label: 'Free', ratio: null },
  { label: 'Original', ratio: 0 },
  { label: 'Square', ratio: 1 },
  { label: '4:3', ratio: 4 / 3 },
  { label: '3:2', ratio: 3 / 2 },
  { label: '16:9', ratio: 16 / 9 },
  { label: '4:5', ratio: 4 / 5 },
  { label: '9:16', ratio: 9 / 16 },
]

/** Size of the turned picture (before cropping) for a photo of `w` x `h` as displayed. */
export function baseSize(w: number, h: number, rotate: number): [number, number] {
  return rotate % 2 === 1 ? [h, w] : [w, h]
}

/** Size after turning and cropping. */
export function outputSize(e: Edit, w: number, h: number): [number, number] {
  const [bw, bh] = baseSize(w, h, e.rotate)
  const c = e.crop ?? { x: 0, y: 0, w: 1, h: 1 }
  return [Math.max(1, Math.round(c.w * bw)), Math.max(1, Math.round(c.h * bh))]
}

/**
 * Whether every corner of the crop (fractions of a `bw` x `bh` frame) shows the picture after
 * it is turned by `deg` around the centre.
 */
export function cropFits(c: Crop, bw: number, bh: number, deg: number): boolean {
  const a = (deg * Math.PI) / 180
  const cos = Math.cos(a)
  const sin = Math.sin(a)
  const eps = 1e-6
  for (const [u, v] of [
    [c.x, c.y],
    [c.x + c.w, c.y],
    [c.x, c.y + c.h],
    [c.x + c.w, c.y + c.h],
  ]) {
    const dx = (u - 0.5) * bw
    const dy = (v - 0.5) * bh
    // Turned back: where this corner falls in the unturned picture.
    const qx = cos * dx + sin * dy
    const qy = -sin * dx + cos * dy
    if (Math.abs(qx) > bw / 2 + eps || Math.abs(qy) > bh / 2 + eps) return false
  }
  return c.x >= -eps && c.y >= -eps && c.x + c.w <= 1 + eps && c.y + c.h <= 1 + eps
}

/** The crop shrunk around its centre until it fits the turned picture. */
export function fitCrop(c: Crop, bw: number, bh: number, deg: number): Crop {
  const clamp = (c: Crop): Crop => {
    const w = Math.min(c.w, 1)
    const h = Math.min(c.h, 1)
    return { w, h, x: Math.min(Math.max(c.x, 0), 1 - w), y: Math.min(Math.max(c.y, 0), 1 - h) }
  }
  let base = clamp(c)
  if (cropFits(base, bw, bh, deg)) return base
  const cx = base.x + base.w / 2
  const cy = base.y + base.h / 2
  const scaled = (k: number) => clamp({ x: cx - (base.w * k) / 2, y: cy - (base.h * k) / 2, w: base.w * k, h: base.h * k })
  let lo = 0
  let hi = 1
  for (let i = 0; i < 30; i++) {
    const mid = (lo + hi) / 2
    if (cropFits(scaled(mid), bw, bh, deg)) lo = mid
    else hi = mid
  }
  // Centred on the picture if even a tiny crop around this centre doesn't fit.
  if (lo < 0.02) {
    base = { ...base, x: 0.5 - base.w / 2, y: 0.5 - base.h / 2 }
    return fitCrop(base, bw, bh, deg)
  }
  return scaled(lo)
}

/** The biggest crop with pixel aspect `ratio` centred where `around` is, inside the frame. */
export function cropWithAspect(ratio: number, around: Crop, bw: number, bh: number): Crop {
  // In fractions: w * bw / (h * bh) = ratio.
  let w = 1
  let h = (w * bw) / (bh * ratio)
  if (h > 1) {
    h = 1
    w = (h * bh * ratio) / bw
  }
  const cx = around.x + around.w / 2
  const cy = around.y + around.h / 2
  return {
    w,
    h,
    x: Math.min(Math.max(cx - w / 2, 0), 1 - w),
    y: Math.min(Math.max(cy - h / 2, 0), 1 - h),
  }
}

/** A quarter turn clockwise, keeping the same part of the picture cropped. */
export function turned(e: Edit): Edit {
  const c = e.crop
  return {
    ...e,
    rotate: (e.rotate + 1) % 4,
    crop: c ? { x: 1 - (c.y + c.h), y: c.x, w: c.h, h: c.w } : null,
  }
}

/** Mirrored left-right as it is seen now (whatever the turns), keeping the crop on the same part. */
export function mirrored(e: Edit): Edit {
  const c = e.crop
  return {
    ...e,
    flip: !e.flip,
    // Mirroring reverses the direction of the turns.
    rotate: (4 - e.rotate) % 4,
    straighten: -e.straighten,
    crop: c ? { ...c, x: 1 - c.x - c.w } : null,
  }
}

// ---------- server ----------

/** The photo with an unsaved edit, as a JPEG blob, at most `max` px on its longest side. */
export async function previewBlob(id: number, edit: Edit, max: number, signal?: AbortSignal): Promise<Blob> {
  const { baseUrl, token } = await serverInfo()
  const res = await fetch(`${baseUrl}/api/media/${id}/edit/preview`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
    body: JSON.stringify({ edit, max: Math.round(max) }),
    signal,
  })
  if (!res.ok) {
    let message = res.statusText
    try {
      message = (await res.json()).error ?? message
    } catch {
      // not JSON
    }
    throw new ApiError(res.status, message)
  }
  return res.blob()
}
