/**
 * How much the interface animates. The setting is `auto | high | medium | none`; `auto` becomes
 * `high` when the webview renders on a graphics card, `medium` when it falls back to software
 * rendering, and `none` when Windows animation effects are turned off.
 *
 * The level is written to `<html data-motion>`, where app.css picks durations, distances and
 * extras (blur, stagger, springy easing) from it. Svelte transitions run outside CSS, so they
 * read `motion.level` through the helpers below.
 */
import { cubicOut, backOut } from 'svelte/easing'
import type { TransitionConfig } from 'svelte/transition'
import type { Animations } from './api'

export type MotionLevel = 'high' | 'medium' | 'none'

const CACHE_KEY = 'memora.animations'

let gpuCache: boolean | undefined

/** True when WebGL runs on real graphics hardware (not SwiftShader / the Basic Render Driver). */
export function hasGpu(): boolean {
  if (gpuCache !== undefined) return gpuCache
  gpuCache = false
  try {
    const canvas = document.createElement('canvas')
    // `failIfMajorPerformanceCaveat` refuses a context when only software rendering is available.
    const gl = (canvas.getContext('webgl2', { failIfMajorPerformanceCaveat: true }) ??
      canvas.getContext('webgl', { failIfMajorPerformanceCaveat: true })) as WebGLRenderingContext | null
    if (gl) {
      const info = gl.getExtension('WEBGL_debug_renderer_info')
      const renderer = String(info ? gl.getParameter(info.UNMASKED_RENDERER_WEBGL) : gl.getParameter(gl.RENDERER))
      gpuCache = !/swiftshader|basic render|llvmpipe|software|microsoft basic/i.test(renderer)
      gl.getExtension('WEBGL_lose_context')?.loseContext()
    }
  } catch {
    gpuCache = false
  }
  return gpuCache
}

function osReducedMotion(): boolean {
  return window.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false
}

/** The level `auto` stands for on this PC right now. */
export function autoLevel(): MotionLevel {
  if (osReducedMotion()) return 'none'
  return hasGpu() ? 'high' : 'medium'
}

function resolve(setting: Animations): MotionLevel {
  return setting === 'auto' ? autoLevel() : setting
}

function readCache(): Animations {
  try {
    const v = localStorage.getItem(CACHE_KEY)
    if (v === 'auto' || v === 'high' || v === 'medium' || v === 'none') return v
  } catch {
    // storage unavailable: use the default
  }
  return 'auto'
}

export const motion = $state<{ setting: Animations; level: MotionLevel }>({ setting: 'auto', level: 'medium' })

/** Apply the setting. Also cached locally, so the next start animates right from the first frame. */
export function applyAnimations(setting: Animations) {
  motion.setting = setting
  motion.level = resolve(setting)
  document.documentElement.dataset.motion = motion.level
  try {
    localStorage.setItem(CACHE_KEY, setting)
  } catch {
    // not important
  }
}

/** Call once at startup, before the first render. */
export function initMotion() {
  applyAnimations(readCache())
  // Follow Windows' "Animation effects" switch while on `auto`.
  window.matchMedia?.('(prefers-reduced-motion: reduce)').addEventListener?.('change', () => {
    if (motion.setting === 'auto') applyAnimations('auto')
  })
}

// ---------- Svelte transitions ----------

const SCALE: Record<MotionLevel, number> = { high: 1, medium: 0.6, none: 0 }

/** Fade + rise + scale: dialogs, menus, toasts. `y` is the rise in px at the high level. */
export function pop(
  _node: Element,
  { y = 8, scale = 0.94, duration = 240, delay = 0 }: { y?: number; scale?: number; duration?: number; delay?: number } = {},
): TransitionConfig {
  const level = motion.level
  const k = SCALE[level]
  if (k === 0) return { duration: 0 }
  const s0 = 1 - (1 - scale) * (level === 'high' ? 1 : 0.4)
  const dy = y * (level === 'high' ? 1 : 0.5)
  return {
    delay,
    duration: duration * k,
    easing: level === 'high' ? backOut : cubicOut,
    css: (t, u) => `opacity:${Math.min(1, t * 1.6)};transform:translateY(${u * dy}px) scale(${s0 + (1 - s0) * t})`,
  }
}

/** Plain fade (backdrops), scaled to the level. */
export function fadeMotion(_node: Element, { duration = 200 }: { duration?: number } = {}): TransitionConfig {
  const k = SCALE[motion.level]
  if (k === 0) return { duration: 0 }
  return { duration: duration * k, easing: cubicOut, css: (t) => `opacity:${t}` }
}

/**
 * Next / Previous in the viewers: the new photo glides in from the side it comes from while
 * the old one slides out the other way. The new one starts partly visible, so there is never
 * a black gap, and nothing waits for the animation: the next click works right away.
 */
export function slidePhoto(
  node: Element,
  { dir = 1, out = false }: { dir?: number; out?: boolean } = {},
): TransitionConfig {
  const level = motion.level
  // Videos don't slide: that would drag the control bar along (and keep the old one playing
  // while it leaves). The old video goes at once; the new picture fades in through CSS.
  if (level === 'none' || node.querySelector('.vp')) return { duration: 0 }
  const high = level === 'high'
  const dist = high ? 90 : 36
  const duration = (high ? 340 : 200) * (out ? 0.8 : 1)
  return {
    duration,
    easing: cubicOut,
    css: out
      ? (t, u) => `pointer-events:none;opacity:${t};transform:translateX(${-dir * u * dist}px) scale(${1 - u * (high ? 0.04 : 0)})`
      : (t, u) => `opacity:${0.35 + 0.65 * t};transform:translateX(${dir * u * dist}px) scale(${1 - u * (high ? 0.03 : 0)})`,
  }
}
