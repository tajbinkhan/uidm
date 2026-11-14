<!-- About UDM: the version, and the daemon's. -->
<script lang="ts">
  import { checkUpdate, closeWindow, installUpdate, messageOf } from '../../ipc';
  import type { AboutView } from '../../ipc/types';
  import { projection } from '../../stores/projection.svelte';
  import { t } from '../../i18n';
  import Window from '../../components/Window.svelte';
  import logo from '../../../../assets/icon-256.png';

  const view: AboutView | null = $derived(projection.view?.window === 'about' ? projection.view.view : null);
  let error = $state('');

  async function run(p: () => Promise<unknown>) {
    error = '';
    try {
      await p();
    } catch (e) {
      error = messageOf(e);
    }
  }
</script>

<Window onescape={() => void closeWindow()}>
  {#if view}
    <div class="dialog">
      <div class="head">
        <img src={logo} width="40" height="40" alt="" />
        <div class="titles">
          <h1>{t('Unified Download Manager')}</h1>
          <p class="version">{t('Version: {version}', { version: view.version })}</p>
        </div>
      </div>
      <button class="btn check" disabled={!view.connected || view.checking} onclick={() => void run(checkUpdate)}>{t('Check for Update')}</button>
      {#if view.update || error}
        <div class="row update" role="status">
          <span class="grow" class:error-text={!!error}>{error || view.update}</span>
          {#if view.canInstall}
            <button
              class="btn"
              disabled={!view.connected}
              title={view.installTitle}
              onclick={() => void run(installUpdate)}>{t('Update now')}</button
            >
          {/if}
        </div>
      {/if}
      <div class="box">
        <p>{t('UDM is free software under the MIT license.')}</p>
        {#if view.daemon}<p>{t('Background service: version {version}', { version: view.daemon })}</p>{/if}
        <p class="credit">
          {t('Server countries come from ip-location-db, with data from the Number Resource Organization (nro.net) under the CC BY 4.0 license.')}
        </p>
      </div>
    </div>
  {/if}
</Window>

<style>
  .dialog {
    padding: 6px 14px 14px;
    gap: 4px;
  }

  .head {
    display: flex;
    gap: 10px;
    align-items: flex-start;
  }

  img {
    margin-top: 4px;
  }

  h1 {
    margin: 0;
    font-size: 19px;
    font-weight: 700;
  }

  .version {
    margin: 2px 0 0 52px;
    color: var(--link);
  }

  .check {
    align-self: center;
    width: 147px;
  }

  .update {
    margin-top: 4px;
  }

  .box {
    margin-top: 8px;
    min-height: 74px;
    padding: 6px 8px;
    border: 1px solid var(--border);
  }

  .box p {
    margin: 0 0 4px;
  }

  .credit {
    font-size: 11px;
    color: var(--fg-muted);
  }
</style>
