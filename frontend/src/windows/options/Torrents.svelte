<!-- Options > Torrents: the port peers reach UDM on, how peers are found,
     how many and how fast, when finished torrents stop seeding, and whether
     .torrent files from the browser are taken as torrents. -->
<script lang="ts">
  import { t } from '../../i18n';
  import { editing } from './draft.svelte';
  import PageHead from './PageHead.svelte';

  // The pages are drawn only while the draft exists.
  const e = $derived(editing.draft!.extras);
</script>

<PageHead icon="download" title={t('Torrents and magnet links')} />
<div class="opt-col">
  <fieldset class="group opt-col">
    <legend>{t('Connections')}</legend>
    <label class="check"><input type="radio" name="port" checked={!e.torrentPortFixed} onchange={() => (e.torrentPortFixed = false)} />{t('Take a free port each time UDM starts')}</label>
    <div class="pairs">
      <label class="check"><input type="radio" name="port" checked={e.torrentPortFixed} onchange={() => (e.torrentPortFixed = true)} />{t('Listen on port')}</label>
      <input class="text num" type="number" min="1" max="65535" bind:value={e.torrentPort} disabled={!e.torrentPortFixed} aria-label={t('Listen on port')} />
      <label class="check"><input type="checkbox" bind:checked={e.torrentPeersOn} />{t('Peers per torrent at most')}</label>
      <input class="text num" type="number" min="1" max="1000" bind:value={e.torrentPeers} disabled={!e.torrentPeersOn} aria-label={t('Peers per torrent at most')} />
      <label class="check"><input type="checkbox" bind:checked={e.torrentUploadOn} />{t('Upload at most')}</label>
      <span class="row"
        ><input class="text num" type="number" min="1" bind:value={e.torrentUploadKb} disabled={!e.torrentUploadOn} aria-label={t('Upload at most')} /><span
          >{t('KBytes/sec')}</span
        ></span
      >
    </div>
    <label class="check"><input type="checkbox" bind:checked={e.torrentDht} />{t('Find peers through the DHT, without a tracker')}</label>
    <label class="check"><input type="checkbox" bind:checked={e.torrentUpnp} />{t('Ask the router to forward the port (UPnP)')}</label>
    <label class="check"><input type="checkbox" bind:checked={e.torrentLocalPeers} />{t('Find peers on the local network')}</label>
    {#if e.adapter}
      <p class="opt-note">{t('Torrents go through the adapter chosen in Options > Network, without UPnP or the local network search.')}</p>
    {/if}
  </fieldset>
  <fieldset class="group opt-col">
    <legend>{t('Seeding: stop at the first limit reached')}</legend>
    <div class="pairs">
      <label class="check"><input type="checkbox" bind:checked={e.seedRatioOn} />{t('Stop at a share ratio of')}</label>
      <input class="text num" type="number" min="0" max="100" step="0.1" bind:value={e.seedRatio} disabled={!e.seedRatioOn} aria-label={t('Stop at a share ratio of')} />
      <label class="check"><input type="checkbox" bind:checked={e.seedMinutesOn} />{t('Stop after this many minutes')}</label>
      <input class="text num" type="number" min="0" bind:value={e.seedMinutes} disabled={!e.seedMinutesOn} aria-label={t('Stop after this many minutes')} />
    </div>
  </fieldset>
  <label class="check"><input type="checkbox" bind:checked={e.catchTorrentFiles} />{t('Take .torrent files the browser downloads as torrents')}</label>
</div>

<style>
  .pairs {
    display: grid;
    grid-template-columns: max-content max-content;
    align-items: center;
    gap: 3px 10px;
  }

  .num {
    width: 80px;
    text-align: right;
  }

  p {
    margin: 0;
  }
</style>
