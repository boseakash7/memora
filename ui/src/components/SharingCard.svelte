<script lang="ts">
  import { untrack } from 'svelte'
  import { api, errorMessage, qrUrl, type LanStatus, type Settings } from '../lib/api'
  import Icon from '../lib/Icon.svelte'

  let { settings, onchange }: { settings: Settings; onchange: (s: Settings) => void } = $props()

  let status = $state<LanStatus | null>(null)
  let error = $state('')
  let busy = $state(false)
  // Text fields are saved on Enter / leaving the field, not on every key.
  let name = $state(untrack(() => settings.deviceName))
  let pin = $state(untrack(() => settings.tvPin ?? ''))
  let port = $state(untrack(() => String(settings.lanPort)))

  async function refresh() {
    try {
      status = await api.lan()
    } catch (e) {
      error = errorMessage(e)
    }
  }

  $effect(() => {
    refresh()
  })

  async function save(patch: Partial<Settings>) {
    error = ''
    busy = true
    try {
      const s = await api.settings.update(patch)
      onchange(s)
      name = s.deviceName
      pin = s.tvPin ?? ''
      port = String(s.lanPort)
    } catch (e) {
      error = errorMessage(e)
    } finally {
      busy = false
    }
    await refresh()
  }

  function saveName() {
    if (name.trim() !== settings.deviceName) save({ deviceName: name.trim() })
  }

  function savePin() {
    const value = pin.trim()
    if (value === (settings.tvPin ?? '')) return
    save({ tvPin: value === '' ? null : value })
  }

  function savePort() {
    const n = Number(port)
    if (n !== settings.lanPort) save({ lanPort: Number.isInteger(n) ? n : -1 })
  }

  const onEnter = (fn: () => void) => (e: KeyboardEvent) => {
    if (e.key === 'Enter') fn()
  }

  const addresses = $derived(status ? status.addresses.filter((a) => !a.isVirtual) : [])
  const others = $derived(status ? status.addresses.filter((a) => a.isVirtual) : [])
  const tvUrl = (ip: string) => `http://${ip}:${status?.port}/tv/`
</script>

<div class="card">
  <div class="card-head">
    <div>
      <h2>TV and network sharing</h2>
      <p class="muted">
        Show your photos and videos on a TV or phone on your home network. Nothing is sent to the internet, and
        nobody can change or delete anything from there.
      </p>
    </div>
    <label class="switch" title={settings.lanSharing ? 'Stop sharing' : 'Start sharing'}>
      <input
        type="checkbox"
        checked={settings.lanSharing}
        disabled={busy}
        onchange={(e) => save({ lanSharing: e.currentTarget.checked })}
      />
      <span class="slider"></span>
    </label>
  </div>

  {#if error}<p class="error">{error}</p>{/if}

  {#if settings.lanSharing && status}
    {#if status.error}
      <p class="error">{status.error}</p>
    {:else if status.running}
      <div class="share-status">
        {#if status.tvPage && addresses.length > 0}
          <div class="share-main">
          <div class="share-urls">
            <p><strong>On a TV browser or phone, open:</strong></p>
            {#each addresses as a (a.ip)}
              <p class="share-url">
                <code>{tvUrl(a.ip)}</code>
                <span class="muted">({a.name})</span>
              </p>
            {/each}
            {#if status.dlna}
              <p class="muted">
                On a smart TV you can also open its Source or Media menu and choose “{settings.deviceName}”.
              </p>
            {/if}
          </div>
          <img class="qr" src={qrUrl(tvUrl(addresses[0].ip))} alt="QR code for {tvUrl(addresses[0].ip)}" />
          </div>
        {:else if status.dlna}
          <p>
            <Icon name="check" size={18} /> On the TV, open its Source or Media menu and choose
            <strong>“{settings.deviceName}”</strong>.
          </p>
        {/if}
        {#if addresses.length === 0}
          <p class="muted">This PC doesn't seem to be connected to a network.</p>
        {/if}
        {#if others.length > 0}
          <p class="muted small">
            Also listening on virtual adapters ({others.map((a) => a.name).join(', ')}); TVs can't reach those.
          </p>
        {/if}
      </div>
    {/if}

    <div class="options">
      <label class="check">
        <input
          type="checkbox"
          checked={settings.lanTvPage}
          disabled={busy}
          onchange={(e) => save({ lanTvPage: e.currentTarget.checked })}
        />
        <span>
          TV web page
          <span class="muted">Big tiles, remote-control navigation and a slideshow, in any web browser.</span>
        </span>
      </label>
      <label class="check">
        <input
          type="checkbox"
          checked={settings.lanDlna}
          disabled={busy}
          onchange={(e) => save({ lanDlna: e.currentTarget.checked })}
        />
        <span>
          DLNA media server
          <span class="muted">
            TVs list this PC as a media source. TVs can't enter a PIN, so anyone on your Wi-Fi can browse the
            library this way.
          </span>
        </span>
      </label>
      <label class="check">
        <input
          type="checkbox"
          checked={settings.transcodeVideos}
          disabled={busy}
          onchange={(e) => save({ transcodeVideos: e.currentTarget.checked })}
        />
        <span>
          Convert videos the TV can't play
          <span class="muted">Converted while you watch, using the graphics card if possible.</span>
        </span>
      </label>

      <div class="fields">
        <label>
          <span>Name on the TV</span>
          <input class="text" maxlength="64" bind:value={name} onblur={saveName} onkeydown={onEnter(saveName)} />
        </label>
        <label>
          <span>PIN for the web page</span>
          <input
            class="text pin"
            inputmode="numeric"
            maxlength="8"
            placeholder="None"
            bind:value={pin}
            onblur={savePin}
            onkeydown={onEnter(savePin)}
          />
        </label>
        <label>
          <span>Port</span>
          <input class="text port" inputmode="numeric" bind:value={port} onblur={savePort} onkeydown={onEnter(savePort)} />
        </label>
      </div>
      <p class="muted small">
        The PIN (4–8 digits) is asked once per device. When Windows asks whether to allow Memora on networks,
        allow it on private networks, or TVs won't be able to connect.
      </p>
    </div>
  {/if}
</div>

<style>
  .switch {
    position: relative;
    flex: none;
    width: 44px;
    height: 24px;
    margin-top: 4px;
  }

  .switch input {
    opacity: 0;
    width: 0;
    height: 0;
  }

  .slider {
    position: absolute;
    inset: 0;
    border-radius: 12px;
    background: var(--border);
    cursor: pointer;
    transition: background 0.15s;
  }

  .slider::before {
    content: '';
    position: absolute;
    left: 3px;
    top: 3px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: #fff;
    transition: transform 0.15s;
  }

  .switch input:checked + .slider {
    background: var(--accent);
  }

  .switch input:checked + .slider::before {
    transform: translateX(20px);
  }

  .switch input:focus-visible + .slider {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .share-status {
    margin: 8px 0 16px;
    padding: 16px;
    border-radius: 10px;
    background: var(--bg-elev);
  }

  .share-main {
    display: flex;
    align-items: flex-start;
    gap: 24px;
  }

  .share-urls {
    flex: 1;
    min-width: 0;
  }

  .share-urls p,
  .share-status > p {
    margin: 0 0 6px;
  }

  .share-status > p.small {
    margin-top: 10px;
  }

  .share-url code {
    font-size: 15px;
    user-select: all;
  }

  .qr {
    flex: none;
    width: 128px;
    height: 128px;
    border-radius: 8px;
    background: #fff;
  }

  .options {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin-top: 8px;
  }

  .check {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    cursor: pointer;
  }

  .check input {
    accent-color: var(--accent);
    width: 18px;
    height: 18px;
    margin: 2px 0 0;
    flex: none;
  }

  .check .muted {
    display: block;
    font-size: 13px;
  }

  .fields {
    display: flex;
    flex-wrap: wrap;
    gap: 16px;
    margin-top: 4px;
  }

  .fields label {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .text {
    height: 34px;
    width: 260px;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
  }

  .text.pin {
    width: 120px;
    letter-spacing: 2px;
  }

  .text.port {
    width: 90px;
  }

  .small {
    font-size: 13px;
    margin: 0;
  }
</style>
