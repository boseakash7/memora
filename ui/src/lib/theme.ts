import type { Theme } from './api'

/** Apply the theme; `system` follows the Windows light/dark setting via CSS. */
export function applyTheme(theme: Theme) {
  document.documentElement.dataset.theme = theme
}
