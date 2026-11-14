<!-- A torrent download window's own tabs: its files (to tick or untick),
     its peers, its trackers, its piece map, and its seeding limits. -->
<script lang="ts">
  import { messageOf, pickTorrentFiles, setSeedLimits, stopSeeding } from '../../ipc';
  import type { TorrentView } from '../../ipc/types';
  import { t } from '../../i18n';
  import ProgressBar from '../../components/ProgressBar.svelte';

  let { torrent, tab }: { torrent: TorrentView; tab: string } = $props();

  /** The seeding limits as the user edits them, taken from the view once. */
  let limits = $state({ own: false, ratioOn: true, ratio: 1, minutesOn: true, minutes: 60 });
  let loaded = $state(false);
  let error = $state('');
  let saved = $state(false);

  $effect(() => {
    if (!loaded) {
      const l = torrent.limits;
      limits = { own: l.own, ratioOn: l.ratioOn, ratio: l.ratio, minutesOn: l.minutesOn, minutes: l.minutes };
      loaded = true;
    }
  });

  async function run(p: Promise<unknown>): Promise<boolean> {
    error = '';
    try {
      await p;
      return true;
    } catch (e) {
      error = messageOf(e);
      return false;
    }
  }

  async function apply() {
    saved = false;
    saved = await run(
      setSeedLimits({
        own: limits.own,
        ratioOn: limits.ratioOn,
        ratio: Number(limits.ratio) || 0,
        minutesOn: limits.minutesOn,
        minutes: Math.max(0, Math.round(Number(limits.minutes) || 0)),
      }),
    );
  }
</script>

{#if tab === 'files'}
  {#if torrent.filesNote}<p class="note" role="status">{torrent.filesNote}</p>{/if}
  <div class="list">
    <table>
      <thead>
        <tr>
          <th scope="col">{t('File')}</th>
          <th scope="col" class="num">{t('Size')}</th>
          <th scope="col" class="bar">{t('Progress')}</th>
        </tr>
      </thead>
      <tbody>
        {#each torrent.files as f (f.index)}
          <tr>
            <td class="path">
              <label class="check" title={f.path}
                ><input
                  type="checkbox"
                  checked={f.chosen}
                  disabled={!torrent.canChoose}
                  onchange={(e) => void run(pickTorrentFiles([f.index], e.currentTarget.checked))}
                />{f.path}</label
              >
            </td>
            <td class="num">{f.size}</td>
            <td class="bar">
              {#if f.fraction != null}
                <span class="progress"><ProgressBar fraction={f.fraction} height={8} /></span><span class="pct">{f.progress}</span>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{:else if tab === 'peers'}
  <p class="line">{torrent.peersLine}</p>
  <div class="list">
    <table>
      <thead>
        <tr>
          <th scope="col">{t('Address')}</th>
          <th scope="col">{t('Client')}</th>
          <th scope="col" class="num">{t('Download speed')}</th>
          <th scope="col" class="num">{t('Upload speed')}</th>
          <th scope="col" class="num">{t('Downloaded')}</th>
          <th scope="col" class="num">{t('Uploaded')}</th>
          <th scope="col" class="short">{t('Connection')}</th>
        </tr>
      </thead>
      <tbody>
        {#each torrent.peers as p (p.address)}
          <tr>
            <td class="cut" title={p.address}>{p.address}</td>
            <td class="cut" title={p.client}>{p.client}</td>
            <td class="num">{p.downSpeed}</td>
            <td class="num">{p.upSpeed}</td>
            <td class="num">{p.downloaded}</td>
            <td class="num">{p.uploaded}</td>
            <td class="short">{p.transport}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{:else if tab === 'trackers'}
  <div class="list">
    <table>
      <thead>
        <tr>
          <th scope="col">{t('Tracker')}</th>
          <th scope="col" class="state">{t('Status')}</th>
        </tr>
      </thead>
      <tbody>
        {#each torrent.trackers as tr, i (i)}
          <tr>
            <td class="cut" title={tr.url}>{tr.url}</td>
            <td class="state" class:muted={tr.skipped}>{tr.state}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
  {#if torrent.private}<p class="note">{t('A private torrent finds peers through its trackers only.')}</p>{/if}
{:else if tab === 'pieces'}
  <p class="line">{torrent.piecesLine}</p>
  <div class="pieces" role="img" aria-label={`${t('Pieces')}: ${torrent.piecesLine}`}>
    {#each torrent.pieces as cell, i (i)}
      <span class="cell" style:--have="{cell}%"></span>
    {/each}
  </div>
{:else if tab === 'seeding'}
  <p class="note">{t('A finished torrent shares what it downloaded with other people, then stops by itself at the first limit it reaches.')}</p>
  <label class="check"
    ><input type="radio" name="limits" checked={!limits.own} onchange={() => (limits.own = false)} />{t('Use the limits in Options: {limits}', {
      limits: torrent.limits.defaults,
    })}</label
  >
  <label class="check"><input type="radio" name="limits" checked={limits.own} onchange={() => (limits.own = true)} />{t('Use its own limits')}</label>
  <div class="own">
    <label class="check"><input type="checkbox" bind:checked={limits.ratioOn} disabled={!limits.own} />{t('Stop at a share ratio of')}</label>
    <input
      class="text small-num"
      type="number"
      min="0"
      max="100"
      step="0.1"
      bind:value={limits.ratio}
      disabled={!limits.own || !limits.ratioOn}
      aria-label={t('Stop at a share ratio of')}
    />
    <label class="check"><input type="checkbox" bind:checked={limits.minutesOn} disabled={!limits.own} />{t('Stop after this many minutes')}</label>
    <input
      class="text small-num"
      type="number"
      min="0"
      step="1"
      bind:value={limits.minutes}
      disabled={!limits.own || !limits.minutesOn}
      aria-label={t('Stop after this many minutes')}
    />
  </div>
  <div class="row foot">
    <button class="btn" onclick={() => void apply()}>{t('Apply')}</button>
    {#if saved}<span class="note" role="status">{t('Saved')}</span>{/if}
    <span class="grow"></span>
    <button class="btn" disabled={!torrent.seeding} onclick={() => void run(stopSeeding())}>{t('Stop seeding')}</button>
  </div>
  {#if torrent.limits.progress}<p class="note">{torrent.limits.progress}</p>{/if}
{/if}
{#if error}<p class="error-text" role="alert">{error}</p>{/if}

<style>
  p {
    margin: 0;
  }

  .note,
  .line {
    font-size: 11px;
    color: var(--fg-muted);
  }

  .list {
    flex: 1;
    min-height: 0;
    overflow: auto;
    background: var(--list);
    border: 1px solid var(--frame);
  }

  table {
    width: 100%;
    border-collapse: collapse;
    table-layout: fixed;
  }

  th {
    position: sticky;
    top: 0;
    z-index: 1;
    background: var(--header);
    font-weight: normal;
    text-align: left;
    padding: 0 5px;
    border-right: 1px solid var(--grid);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  td {
    padding: 0 5px;
    line-height: 18px;
    white-space: nowrap;
  }

  .path,
  .cut {
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .path label {
    display: inline;
  }

  .num {
    width: 86px;
    text-align: right;
  }

  .short {
    width: 72px;
  }

  .state {
    width: 96px;
  }

  .muted {
    color: var(--fg-muted);
  }

  .bar {
    width: 140px;
  }

  .progress {
    display: inline-block;
    width: 74px;
    vertical-align: middle;
    margin-right: 6px;
  }

  .pct {
    font-size: 11px;
  }

  .pieces {
    display: grid;
    grid-template-columns: repeat(32, 1fr);
    gap: 1px;
    padding: 4px;
    background: var(--list);
    border: 1px solid var(--frame);
  }

  .cell {
    height: 12px;
    background: color-mix(in srgb, var(--progress) var(--have), var(--progress-track));
  }

  .own {
    display: grid;
    grid-template-columns: max-content 90px;
    align-items: center;
    gap: 4px 8px;
    padding-left: 22px;
  }

  .small-num {
    width: 90px;
    text-align: right;
  }

  .foot {
    margin-top: auto;
    align-items: center;
  }
</style>
