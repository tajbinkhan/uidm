<!-- Options > Network: the network adapter every download goes through
     (with a kill switch when it is down), and where host names are looked
     up. The shell lists the adapters and names each resolver. -->
<script lang="ts">
  import type { OptionsView } from '../../ipc/types';
  import { t } from '../../i18n';
  import { editing } from './draft.svelte';
  import PageHead from './PageHead.svelte';

  let { view }: { view: OptionsView } = $props();
  // The pages are drawn only while the draft exists.
  const e = $derived(editing.draft!.extras);
  const n = $derived(view.network);
  const chosen = $derived(e.adapter ? n.adapters?.find((a) => a.id === e.adapter?.id) : undefined);

  function pick(id: string) {
    const row = n.adapters?.find((a) => a.id === id);
    e.adapter = row ? { id: row.id, name: row.name } : null;
  }
</script>

<PageHead icon="network" title={t('Network adapter and DNS')} />
<div class="opt-col">
  <p class="opt-note">
    {t('Downloads can go through one network adapter only, such as a VPN’s. While it is down, they stop instead of using another network.')}
  </p>
  <div class="row">
    <label for="adapter">{t('Download through')}</label>
    <select id="adapter" class="select grow" value={e.adapter?.id ?? ''} onchange={(ev) => pick(ev.currentTarget.value)}>
      <option value="">{t('Any network (the system chooses)')}</option>
      {#if n.adapters}
        {#each n.adapters as a (a.id)}<option value={a.id}>{a.label}</option>{/each}
      {:else if e.adapter}
        <option value={e.adapter.id}>{e.adapter.name || e.adapter.id}</option>
      {/if}
    </select>
  </div>
  {#if !n.adapters}<p class="opt-note">{t('Reading the network adapters…')}</p>{/if}
  {#if n.adaptersError}<p class="error-text" role="alert">{n.adaptersError}</p>{/if}
  {#if e.adapter && chosen && !chosen.up}
    <p class="error-text" role="status">{t('This adapter is not connected now: downloads will not start until it is.')}</p>
  {/if}
  <fieldset class="group opt-col">
    <legend>{t('Look up host names with')}</legend>
    {#each n.resolvers as r (r.value)}
      <label class="check"><input type="radio" name="dns" checked={e.dns === r.value} onchange={() => (e.dns = r.value)} />{r.label}</label>
    {/each}
    <div class="row opt-indent">
      <label for="dns-url" class:disabled={e.dns !== 'custom'}>{t('Address')}</label>
      <input
        id="dns-url"
        class="text grow"
        bind:value={e.dnsUrl}
        disabled={e.dns !== 'custom'}
        placeholder="https://dns.example/dns-query"
        spellcheck="false"
        autocomplete="off"
      />
    </div>
    <p class="opt-note">
      {t('DNS over HTTPS hides the names looked up from the network in between. Names are never looked up another way when it fails.')}
    </p>
  </fieldset>
  {#if e.adapter && e.dns === 'system'}
    <p class="opt-note">{t('The system looks up host names itself, possibly through another adapter. Choose a DNS over HTTPS server to keep lookups on this one.')}</p>
  {/if}
</div>

<style>
  p {
    margin: 0;
    overflow-wrap: anywhere;
  }
</style>
