import type { Theme } from './api'

/** Apply the theme; `system` follows the Windows light/dark setting via CSS. */
export function applyTheme(theme: Theme) {
  document.documentElement.dataset.theme = theme
}

/** Apply a theme the user just picked, crossfading the window when animations are on. */
export function switchTheme(theme: Theme) {
  const doc = document as Document & { startViewTransition?: (update: () => void) => unknown }
  if (document.documentElement.dataset.motion === 'none' || !doc.startViewTransition) applyTheme(theme)
  else doc.startViewTransition(() => applyTheme(theme))
}
