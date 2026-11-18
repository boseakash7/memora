<script lang="ts">
  import MediaPage from '../components/MediaPage.svelte'
  import Icon from '../lib/Icon.svelte'
</script>

<MediaPage
  query={{ scope: 'archive' }}
  title="Archive"
  subtitle="Archived items show up only here. They keep their albums, favorites and people."
>
  {#snippet empty()}
    <div class="empty">
      <div class="empty-icon"><Icon name="archive" size={56} /></div>
      <h2>Nothing archived</h2>
      <p>
        Archive photos you want to keep but not see in Photos, albums, search or on the TV. Select them and choose
        Archive, or press Shift+A in the viewer.
      </p>
    </div>
  {/snippet}
</MediaPage>
