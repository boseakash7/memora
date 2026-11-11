<script lang="ts">
  import { untrack } from 'svelte'
  import { api, faceUrl, type Person } from '../lib/api'
  import Icon from '../lib/Icon.svelte'
  import type { IconName } from '../lib/icons'
  import { library } from '../lib/library.svelte'

  const cards: { href: string; icon: IconName; title: string; text: string }[] = [
    { href: '#/people', icon: 'people', title: 'People', text: 'Photos grouped by who is in them.' },
    { href: '#/map', icon: 'map', title: 'Map', text: 'See where your photos were taken.' },
    { href: '#/places', icon: 'explore', title: 'Places', text: 'Browse by city and country.' },
    { href: '#/search?kind=video', icon: 'videocam', title: 'Videos', text: 'Everything you filmed.' },
  ]

  let people = $state<Person[]>([])

  function load() {
    api.people
      .list()
      .then((p) => (people = p))
      .catch(() => {})
  }

  $effect(() => {
    library.status?.revision
    untrack(load)
  })
</script>

<section class="page">
  <h1>Explore</h1>

  {#if people.length > 0}
    <div class="section-head">
      <h2 class="section-title">People</h2>
      <a class="link" href="#/people">View all</a>
    </div>
    <div class="people-row">
      {#each people.slice(0, 10) as p (p.id)}
        <a class="person-card" href={`#/person/${p.id}`}>
          <span class="person-face">
            {#if p.coverFace !== null}
              <img src={faceUrl(p.coverFace)} alt="" draggable="false" loading="lazy" />
            {:else}
              <Icon name="person" size={40} />
            {/if}
          </span>
          <span class="person-name" class:muted={!p.name}>{p.name ?? ''}</span>
        </a>
      {/each}
    </div>
  {/if}

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
