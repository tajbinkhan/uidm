<!-- Options: the settings in IDM's two rows of tabs. The pages edit a copy;
     OK sends it to the shell, which checks it and saves it. -->
<script lang="ts">
  import { closeWindow, messageOf, saveOptions } from '../../ipc';
  import type { OptionsView } from '../../ipc/types';
  import { projection } from '../../stores/projection.svelte';
  import { t } from '../../i18n';
  import Window from '../../components/Window.svelte';
  import { editing } from './draft.svelte';
  import General from './General.svelte';
  import FileTypes from './FileTypes.svelte';
  import SaveTo from './SaveTo.svelte';
  import Downloads from './Downloads.svelte';
  import Connection from './Connection.svelte';
  import Proxy from './Proxy.svelte';
  import Logins from './Logins.svelte';
  import Sounds from './Sounds.svelte';
  import Speed from './Speed.svelte';
  import Behavior from './Behavior.svelte';
  import Browser from './Browser.svelte';
  import Archives from './Archives.svelte';
  import Tools from './Tools.svelte';
  import Remote from './Remote.svelte';
  import Debrid from './Debrid.svelte';
  import Network from './Network.svelte';
  import Torrents from './Torrents.svelte';

  const view: OptionsView | null = $derived(projection.view?.window === 'options' ? projection.view.view : null);

  type Page =
    | 'general'
    | 'types'
    | 'saveTo'
    | 'downloads'
    | 'connection'
    | 'proxy'
    | 'logins'
    | 'sounds'
    | 'speed'
    | 'behavior'
    | 'browser'
    | 'archives'
    | 'tools'
    | 'remote'
    | 'debrid'
    | 'network'
    | 'torrents';
  // IDM's two rows, and behind them a row of UDM's own pages.
  const rows: { key: Page; label: string }[][] = [
    [
      { key: 'speed', label: 'Speed' },
      { key: 'behavior', label: 'Behavior' },
      { key: 'browser', label: 'Browser' },
      { key: 'archives', label: 'Archives' },
      { key: 'tools', label: 'Tools' },
      { key: 'remote', label: 'Remote' },
      { key: 'debrid', label: 'Debrid' },
    ],
    [
      { key: 'proxy', label: 'Proxy / Socks' },
      { key: 'network', label: 'Network' },
      { key: 'torrents', label: 'Torrents' },
      { key: 'logins', label: 'Sites Logins' },
      { key: 'sounds', label: 'Sounds' },
    ],
    [
      { key: 'general', label: 'General' },
      { key: 'types', label: 'File types' },
      { key: 'saveTo', label: 'Save to' },
      { key: 'downloads', label: 'Downloads' },
      { key: 'connection', label: 'Connection' },
    ],
  ];

  let page = $state<Page>('general');
  let error = $state('');
  let busy = $state(false);
  // The row holding the open page sits next to the page, as IDM's do.
  const ordered = $derived([...rows.filter((r) => !r.some((x) => x.key === page)), ...rows.filter((r) => r.some((x) => x.key === page))]);
  const all = rows.flat();

  $effect(() => {
    if (view && !editing.draft) editing.draft = structuredClone($state.snapshot(view.draft));
  });

  async function ok() {
    if (!editing.draft || busy) return;
    busy = true;
    error = '';
    try {
      await saveOptions($state.snapshot(editing.draft));
    } catch (e) {
      error = messageOf(e);
    } finally {
      busy = false;
    }
  }

  function onkeydown(e: KeyboardEvent, key: Page) {
    const i = all.findIndex((x) => x.key === key);
    const step = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
    if (!step) return;
    e.preventDefault();
    page = all[(i + step + all.length) % all.length].key;
    queueMicrotask(() => document.getElementById(`tab-${page}`)?.focus());
  }
</script>

<Window onescape={() => void closeWindow()}>
  {#if view && editing.draft}
    <div class="options">
      <div class="tabs" role="tablist">
        {#each ordered as row, r (r)}
          <div class="tabrow" class:front={r === ordered.length - 1}>
            {#each row as tab (tab.key)}
              <button
                id="tab-{tab.key}"
                class="tab"
                class:on={page === tab.key}
                role="tab"
                aria-selected={page === tab.key}
                aria-controls="page"
                tabindex={page === tab.key ? 0 : -1}
                onclick={() => (page = tab.key)}
                onkeydown={(e) => onkeydown(e, tab.key)}
              >
                {t(tab.label)}
              </button>
            {/each}
          </div>
        {/each}
      </div>
      <div class="page" id="page" role="tabpanel">
        {#if page === 'general'}
          <General {view} />
        {:else if page === 'types'}
          <FileTypes {view} />
        {:else if page === 'saveTo'}
          <SaveTo />
        {:else if page === 'downloads'}
          <Downloads />
        {:else if page === 'connection'}
          <Connection {view} />
        {:else if page === 'speed'}
          <Speed />
        {:else if page === 'behavior'}
          <Behavior {view} />
        {:else if page === 'browser'}
          <Browser {view} />
        {:else if page === 'archives'}
          <Archives />
        {:else if page === 'tools'}
          <Tools {view} />
        {:else if page === 'remote'}
          <Remote {view} />
        {:else if page === 'debrid'}
          <Debrid {view} />
        {:else if page === 'proxy'}
          <Proxy />
        {:else if page === 'network'}
          <Network {view} />
        {:else if page === 'torrents'}
          <Torrents />
        {:else if page === 'logins'}
          <Logins />
        {:else}
          <Sounds />
        {/if}
      </div>
      {#if error}<p class="error-text" role="alert">{error}</p>{/if}
      <div class="foot">
        <button class="btn default" disabled={busy} onclick={() => void ok()}>{t('OK')}</button>
        <button class="btn" onclick={() => void closeWindow()}>{t('Cancel')}</button>
      </div>
    </div>
  {/if}
</Window>

<style>
  .options {
    display: flex;
    flex-direction: column;
    height: 100%;
    padding: 8px 10px 10px;
  }

  .tabs {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-bottom: 6px;
    flex: none;
  }

  .tabrow {
    display: flex;
    gap: 2px;
  }

  /* IDM's rows of tabs, drawn as Windows 11 draws a pivot: plain text,
     the current page's tab raised with the accent's short bar under it. */
  .tab {
    position: relative;
    flex: 1 1 auto;
    height: 20px;
    padding: 0 6px;
    background: none;
    border: 0;
    border-radius: var(--radius);
    color: var(--fg-muted);
    white-space: nowrap;
    transition: background-color 0.08s ease-out;
  }

  .tab:hover {
    background: var(--hover);
    color: var(--fg);
  }

  .tab.on {
    background: var(--list);
    box-shadow: 0 0 0 1px var(--frame);
    color: var(--fg);
    font-weight: 600;
  }

  .tab.on::after {
    content: '';
    position: absolute;
    left: 50%;
    bottom: 0;
    width: 16px;
    height: 3px;
    margin-left: -8px;
    border-radius: 2px;
    background: var(--accent);
  }

  .page {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 8px 10px;
    background: var(--list);
    border: 1px solid var(--frame);
    border-radius: var(--radius-large);
  }

  .foot {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 10px;
  }

  .foot .btn {
    width: 74px;
  }

  p {
    margin: 6px 0 0;
  }

  /* What the pages share. */
  :global(.opt-col) {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  :global(.opt-indent) {
    padding-left: 18px;
  }

  :global(.opt-rule) {
    height: 0;
    margin: 2px 0;
    border-top: 1px solid var(--border);
  }

  :global(.opt-area) {
    width: 100%;
    min-height: 58px;
    padding: 3px 5px;
    resize: none;
    background: var(--input-bg);
    border: 1px solid var(--input-border);
    border-bottom-color: var(--input-underline);
    border-radius: var(--radius);
    color: var(--fg);
    font: inherit;
    user-select: text;
  }

  :global(.opt-area:focus) {
    border-bottom-color: var(--focus);
    box-shadow: inset 0 -1px 0 var(--focus);
  }


  :global(.opt-right) {
    align-self: flex-end;
  }

  :global(.opt-note) {
    margin: 0;
    color: var(--fg-muted);
  }
</style>
