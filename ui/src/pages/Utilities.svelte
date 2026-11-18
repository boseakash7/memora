<script lang="ts">
  import Icon from '../lib/Icon.svelte'
  import type { IconName } from '../lib/icons'

  const cards: { href: string; icon: IconName; title: string; text: string }[] = [
    {
      href: '#/vault',
      icon: 'lock',
      title: 'Locked folder',
      text: 'Keep private photos and videos encrypted, behind a password.',
    },
    {
      href: '#/duplicates',
      icon: 'utilities',
      title: 'Duplicates',
      text: 'Find identical and look-alike photos and videos, and free up space.',
    },
  ]
</script>

<section class="page">
  <h1>Utilities</h1>
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
