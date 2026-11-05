<script lang="ts">
  import MediaPage from '../components/MediaPage.svelte'
  import Icon from '../lib/Icon.svelte'
</script>

<MediaPage query={{ scope: 'favorites' }} title="Favorites">
  {#snippet empty()}
    <div class="empty">
      <div class="empty-icon"><Icon name="star" size={56} /></div>
      <h2>No favorites yet</h2>
      <p>Click the star in the viewer, or select photos and use the star button, to find them here.</p>
    </div>
  {/snippet}
</MediaPage>
