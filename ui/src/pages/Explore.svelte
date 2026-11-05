<script lang="ts">
  import Icon from '../lib/Icon.svelte'
  import type { IconName } from '../lib/icons'

  const cards: { href: string; icon: IconName; title: string; text: string }[] = [
    { href: '#/map', icon: 'map', title: 'Map', text: 'See where your photos were taken.' },
    { href: '#/places', icon: 'explore', title: 'Places', text: 'Browse by city and country.' },
    { href: '#/search?kind=video', icon: 'videocam', title: 'Videos', text: 'Everything you filmed.' },
  ]
</script>

<section class="page">
  <h1>Explore</h1>
  <div class="tile-grid">
    {#each cards as card (card.href)}
      <a class="tile" href={card.href}>
        <span class="tile-icon"><Icon name={card.icon} size={32} /></span>
        <span class="tile-title">{card.title}</span>
        <span class="muted">{card.text}</span>
      </a>
    {/each}
  </div>
</section>
