<script lang="ts">
  import { untrack } from 'svelte'
  import MediaPage from '../components/MediaPage.svelte'
  import PersonPicker from '../components/PersonPicker.svelte'
  import { api, errorMessage, faceUrl, type Person } from '../lib/api'
  import { confirmDialog, promptDialog } from '../lib/dialogs.svelte'
  import Icon from '../lib/Icon.svelte'
  import { library } from '../lib/library.svelte'
  import { go } from '../lib/router.svelte'
  import { toast } from '../lib/toast.svelte'

  let { id }: { id: number } = $props()

  let person = $state<Person | null>(null)
  let merging = $state(false)

  async function load() {
    try {
      person = await api.people.get(id)
    } catch {
      go('people') // merged away, or nobody left
    }
  }

  $effect(() => {
    id
    untrack(load)
  })

  let seen: number | null = null
  $effect(() => {
    const revision = library.status?.revision ?? null
    untrack(() => {
      if (seen !== null && revision !== seen) load()
      seen = revision
    })
  })

  async function attempt(action: () => Promise<void>) {
    try {
      await action()
    } catch (e) {
      toast(errorMessage(e), { error: true })
    }
  }

  async function rename() {
    if (!person) return
    const name = await promptDialog({
      title: person.name ? 'Edit name' : 'Add a name',
      label: 'Name',
      value: person.name ?? '',
      confirmLabel: 'Save',
    })
    if (name === null) return
    await attempt(async () => {
      person = await api.people.update(id, { name: name.trim() || null })
    })
  }

  async function merge(other: Person) {
    merging = false
    if (!person) return
    const a = person.name ?? 'this person'
    const b = other.name ?? 'the other person'
    const ok = await confirmDialog({
      title: 'Same person?',
      message: `The photos of ${b} are added to ${a}. This can't be undone, but a wrong face can still be removed from a photo's info panel.`,
      confirmLabel: 'Merge',
    })
    if (!ok) return
    await attempt(async () => {
      person = await api.people.merge(id, [other.id])
      toast(`Merged into ${person.name ?? 'this person'}`)
    })
  }

  const toggleHidden = () =>
    attempt(async () => {
      if (!person) return
      const hidden = !person.hidden
      person = await api.people.update(id, { hidden })
      toast(hidden ? 'Hidden from People' : 'Shown in People again', {
        action: {
          label: 'Undo',
          run: () =>
            attempt(async () => {
              person = await api.people.update(id, { hidden: !hidden })
            }),
        },
      })
    })
</script>

{#key id}
  <MediaPage
    query={{ person: id }}
    title={person ? (person.name ?? 'Add a name') : 'Person'}
    subtitle={person ? `${person.count.toLocaleString()} ${person.count === 1 ? 'item' : 'items'}${person.hidden ? ' · hidden' : ''}` : ''}
  >
    {#snippet actions()}
      {#if person?.coverFace != null}
        <img class="person-avatar" src={faceUrl(person.coverFace)} alt="" draggable="false" />
      {/if}
      <button class="btn" onclick={rename}><Icon name="edit" size={20} /> {person?.name ? 'Edit name' : 'Add a name'}</button>
      <button class="btn" onclick={() => (merging = true)} title="Combine with someone who is the same person">
        <Icon name="merge" size={20} /> Merge
      </button>
      <button class="btn" onclick={toggleHidden}>
        <Icon name={person?.hidden ? 'visibility' : 'visibilityOff'} size={20} />
        {person?.hidden ? 'Show' : 'Hide'}
      </button>
    {/snippet}
    {#snippet empty()}
      <div class="empty">
        <div class="empty-icon"><Icon name="person" size={56} /></div>
        <h2>No photos of this person</h2>
        <p>Their photos may be in the trash.</p>
        <button class="btn" onclick={() => go('people')}>All people</button>
      </div>
    {/snippet}
  </MediaPage>
{/key}

{#if merging}
  <PersonPicker title="Merge with…" exclude={id} onpick={merge} onclose={() => (merging = false)} />
{/if}
