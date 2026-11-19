import { ask } from '@tauri-apps/plugin-dialog'
import { api } from './api'

/** Ask whether the folder should carry its library info (`.memora`) when it is added. */
export async function askKeepInfo(path: string): Promise<boolean> {
  const probe = await api.folders.probe(path)
  if (probe.root) {
    const from = probe.pcs.length ? probe.pcs.join(', ') : 'another PC'
    return ask(
      `This folder already has library info from ${from}: favorites, albums, the Archive, the trash and people's names.\n\n` +
        'Use it here, and keep the changes you make on this PC there too?',
      { title: 'Library info found', kind: 'info', okLabel: 'Use it', cancelLabel: 'Only on this PC' },
    )
  }
  return ask(
    'Keep your library info in this folder too?\n\n' +
      `Memora adds a small hidden ".memora" folder with your favorites, albums, Archive, trash and people's names. ` +
      'Another PC that adds this folder (external drive, NAS or a synced folder) then shows the same library and stays in step.\n\n' +
      'Your photos themselves are never changed.',
    { title: 'Library info', kind: 'info', okLabel: 'Keep it in the folder', cancelLabel: 'Only on this PC' },
  )
}
