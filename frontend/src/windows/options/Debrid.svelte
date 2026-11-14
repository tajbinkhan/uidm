<!-- Options > Debrid: a debrid account (experimental) that turns links to
     file hosts into fast direct links. The stored key never comes back to
     the window: the field starts empty, and a key typed there replaces it. -->
<script lang="ts">
  import { checkDebrid, messageOf } from '../../ipc';
  import type { OptionsView } from '../../ipc/types';
  import { t } from '../../i18n';
  import { editing } from './draft.svelte';
  import PageHead from './PageHead.svelte';

  let { view }: { view: OptionsView } = $props();
  // The pages are drawn only while the draft exists.
  const e = $derived(editing.draft!.extras);
  const d = $derived(view.debrid);
  const chosen = $derived(d.services.find((s) => s.service === e.debridService));
  const stored = $derived(d.keyStored && !e.debridForget);
  let error = $state('');

  async function check() {
    if (!e.debridService) return;
    error = '';
    try {
      await checkDebrid(e.debridService, e.debridKey);
    } catch (err) {
      error = messageOf(err);
    }
  }
</script>

<PageHead icon="key" title={t('Debrid account (experimental)')} />
<div class="opt-col">
  <p class="opt-note">
    {t('A debrid service turns links to file hosts it supports into fast direct links for its paying users. While this is on, those links are sent to the chosen service before they download. This is experimental.')}
  </p>
  <label class="check"><input type="checkbox" bind:checked={e.debridOn} />{t("Download file hosts' links through a debrid account")}</label>
  <fieldset class="group opt-col">
    <legend>{t('Service')}</legend>
    {#each d.services as s (s.service)}
      <label class="check"
        ><input type="radio" name="debrid-service" checked={e.debridService === s.service} onchange={() => (e.debridService = s.service)} />{s.name}</label
      >
    {/each}
  </fieldset>
  <div class="row">
    <label for="debrid-key">{t('API key')}</label>
    <input
      id="debrid-key"
      class="text grow"
      type="password"
      bind:value={e.debridKey}
      placeholder={stored ? t('(unchanged)') : ''}
      autocomplete="off"
      spellcheck="false"
    />
  </div>
  {#if chosen}<p class="opt-note">{t('Find the key at {page}', { page: chosen.keyPage })}</p>{/if}
  <p class="opt-note">{stored ? t('A key is stored.') : t('No key is stored.')}</p>
  {#if d.keyStored}
    <label class="check opt-indent"><input type="checkbox" bind:checked={e.debridForget} />{t('Remove the stored key when saving')}</label>
  {/if}
  <div class="row">
    <button class="btn" disabled={!e.debridService || d.checking || (!stored && !e.debridKey.trim())} onclick={() => void check()}
      >{d.checking ? t('Checking…') : t('Check account')}</button
    >
  </div>
  {#if d.check}<p class:error-text={d.check.error} class:opt-note={!d.check.error} role="status">{d.check.text}</p>{/if}
  {#if error}<p class="error-text" role="alert">{error}</p>{/if}
</div>

<style>
  p {
    margin: 0;
    overflow-wrap: anywhere;
  }
</style>
