<!-- A download's progress window: its status, speed limiter and what
     happens when it is done, the progress bar, and with its details the
     connections' ranges. -->
<script lang="ts">
  import { closeWindow, controlDownload, fitHeight, messageOf, setDownloadLimit, setDownloadOnDone } from '../../ipc';
  import type { DownloadView, Then } from '../../ipc/types';
  import { projection } from '../../stores/projection.svelte';
  import { t } from '../../i18n';
  import Window from '../../components/Window.svelte';
  import Tabs from '../../components/Tabs.svelte';
  import ProgressBar from '../../components/ProgressBar.svelte';

  const WIDTH = 552;
  const SHORT = 244;
  const TALL = 413;

  const view: DownloadView | null = $derived(projection.view?.window === 'download' ? projection.view.view : null);

  let tab = $state('status');
  let details: boolean | null = $state(null);
  let hidden = $state<string[]>([]);
  let limit = $state({ on: false, kb: 10, remember: true });
  let onDone = $state({ dialog: false, then: 'nothing' as Then, forceClose: false });
  /** Which way the computer turns off, kept while the box is unticked. */
  let off = $state<Then>('shutDown');
  let loaded = $state(false);
  let error = $state('');

  $effect(() => {
    if (view && !loaded) {
      limit = { on: view.limit.on, kb: view.limit.kbPerSec, remember: view.limit.remember };
      onDone = { ...view.onDone };
      if (view.onDone.then === 'sleep' || view.onDone.then === 'hibernate') off = view.onDone.then;
      details = view.details;
      if (view.details) void fitHeight(WIDTH, TALL);
      loaded = true;
    }
  });

  const tabs = $derived(
    [
      { key: 'status', label: t('Download status') },
      ...(view?.speedTab ? [{ key: 'speed', label: t('Speed Limiter') }] : []),
      ...(view?.doneTab ? [{ key: 'done', label: t('Options on completion') }] : []),
    ].filter((x) => !hidden.includes(x.key)),
  );

  function hide(key: string) {
    hidden = [...hidden, key];
    tab = 'status';
  }

  function toggleDetails() {
    details = !details;
    void fitHeight(WIDTH, details ? TALL : SHORT);
  }

  async function run(p: Promise<unknown>) {
    error = '';
    try {
      await p;
    } catch (e) {
      error = messageOf(e);
    }
  }

  const sendLimit = () => run(setDownloadLimit({ on: limit.on, kbPerSec: Math.round(Number(limit.kb)) || 0, remember: limit.remember }));
  const sendOnDone = () => run(setDownloadOnDone({ dialog: onDone.dialog, then: onDone.then, forceClose: onDone.forceClose }));
  const turnOff = $derived(onDone.then === 'shutDown' || onDone.then === 'sleep' || onDone.then === 'hibernate');
</script>

<Window onescape={() => void closeWindow()}>
  {#if view}
    <div class="dialog">
      <Tabs {tabs} bind:current={tab}>
        <div class="page">
          {#if tab === 'status'}
            <!-- The server's country goes before the address: the page has
                 no room for another row. -->
            <p class="address" title={view.address}>
              {#if view.country}<span class="country" title={t('Server country')}>{view.country}</span>{/if}{view.address}
            </p>
            <dl class="pairs">
              <dt>{t('Status')}</dt>
              <dd>{view.status}</dd>
              <dt class="gap">{t('File size')}</dt>
              <dd class="gap">{view.size}</dd>
              <dt>{t('Downloaded')}</dt>
              <dd>{view.downloaded}</dd>
              <dt>{t('Transfer rate')}</dt>
              <dd>{view.speed}</dd>
              <dt>{t('Time left')}</dt>
              <dd>{view.timeLeft}</dd>
              <dt>{t('Resume capability')}</dt>
              <dd class="resume">{view.resume}</dd>
            </dl>
          {:else if tab === 'speed'}
            <dl class="pairs">
              <dt>{t('Transfer rate')}</dt>
              <dd>{view.speed}</dd>
            </dl>
            <label class="check"><input type="checkbox" bind:checked={limit.on} onchange={sendLimit} />{t('Use Speed Limiter')}</label>
            <div class:disabled={!limit.on}>{t('Maximum download speed:')}</div>
            <div class="row">
              <input
                class="text kb"
                type="number"
                min="1"
                bind:value={limit.kb}
                disabled={!limit.on}
                onchange={sendLimit}
                aria-label={t('Maximum download speed:')}
              />
              <span class:disabled={!limit.on}>{t('KBytes/sec')}</span>
            </div>
            <div class="row foot">
              <label class="check remember">
                <input type="checkbox" bind:checked={limit.remember} onchange={sendLimit} />
                {t('Remember Speed Limiter settings for this file on download stop/resume')}
              </label>
              <button class="btn hide" onclick={() => hide('speed')}>{t('Hide tab')}</button>
            </div>
          {:else}
            <p class="save-to"><span>{t('Save To:')}</span> <span title={view.saveTo}>{view.saveTo}</span></p>
            <label class="check"><input type="checkbox" bind:checked={onDone.dialog} onchange={sendOnDone} />{t('Show download complete dialog')}</label>
            <hr />
            <p class="disabled note">{t('These settings are unavailable when "Show download complete dialog" is turned on')}</p>
            <label class="check">
              <input
                type="checkbox"
                checked={onDone.then === 'exit'}
                disabled={onDone.dialog}
                onchange={(e) => {
                  onDone.then = e.currentTarget.checked ? 'exit' : 'nothing';
                  void sendOnDone();
                }}
              />{t('Exit UDM when done')}
            </label>
            <div class="row foot">
              <label class="check">
                <input
                  type="checkbox"
                  checked={turnOff}
                  disabled={onDone.dialog}
                  onchange={(e) => {
                    onDone.then = e.currentTarget.checked ? off : 'nothing';
                    void sendOnDone();
                  }}
                />{t('Turn off computer when done')}
              </label>
              <select
                class="select then"
                disabled={onDone.dialog || !turnOff}
                bind:value={off}
                onchange={() => {
                  onDone.then = off;
                  void sendOnDone();
                }}
                aria-label={t('Turn off computer when done')}
              >
                <option value="shutDown">{t('Shut down')}</option>
                <option value="sleep">{t('Sleep')}</option>
                <option value="hibernate">{t('Hibernate')}</option>
              </select>
              <button class="btn hide" onclick={() => hide('done')}>{t('Hide tab')}</button>
            </div>
            <label class="check indent">
              <input type="checkbox" bind:checked={onDone.forceClose} disabled={onDone.dialog || !turnOff} onchange={sendOnDone} />
              {t('Force processes to terminate')}
            </label>
          {/if}
        </div>
      </Tabs>
      <ProgressBar fraction={view.fraction} />
      <div class="row controls">
        <button class="btn wide" onclick={toggleDetails}>{details ? t('<< Hide details') : t('Show details >>')}</button>
        <span class="grow"></span>
        {#if view.canResume}
          <button class="btn mid" onclick={() => void run(controlDownload('resume'))}>{t('Resume')}</button>
        {:else}
          <button class="btn mid" disabled={!view.running} onclick={() => void run(controlDownload('pause'))}>{t('Pause')}</button>
        {/if}
        <button class="btn mid" onclick={() => void run(controlDownload('cancel'))}>{t('Cancel')}</button>
      </div>
      {#if error}<p class="error-text" role="alert">{error}</p>{/if}
      {#if details}
        <p class="caption">{t('Start positions and download progress by connections')}</p>
        <div class="strip" aria-hidden="true">
          {#each view.segments as s, i (i)}
            <span class="mark" style:left="{s.start * 100}%"></span>
            <span class="reach" style:left="{s.start * 100}%" style:width="{Math.max(0, s.reached - s.start) * 100}%"></span>
          {/each}
        </div>
        <div class="connections" role="table" aria-label={t('Start positions and download progress by connections')}>
          <div class="head" role="row">
            <span role="columnheader" class="n">{t('N.')}</span>
            <span role="columnheader" class="got">{t('Downloaded')}</span>
            <span role="columnheader">{t('Info')}</span>
          </div>
          <div class="rows">
            {#each view.segments as s, i (i)}
              <div class="r" role="row">
                <span role="cell" class="n">{i + 1}</span>
                <span role="cell" class="got">{s.downloaded}</span>
                <span role="cell">{s.info}</span>
              </div>
            {/each}
          </div>
        </div>
      {/if}
    </div>
  {/if}
</Window>

<style>
  .dialog {
    padding: 8px 18px 8px 18px;
    gap: 9px;
  }

  .page {
    height: 128px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  p {
    margin: 0;
  }

  .address,
  .save-to {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* The labels' column grows for a longer label (a wider font than
     Segoe UI, a longer language) rather than wrap it, which would push the
     address out of the page. */
  .pairs {
    display: grid;
    grid-template-columns: minmax(102px, max-content) 1fr;
    column-gap: 8px;
    margin: 0;
    row-gap: 1px;
  }

  dt,
  dd {
    margin: 0;
  }

  dt {
    white-space: nowrap;
  }

  .gap {
    margin-top: 8px;
  }

  .resume {
    padding-left: 78px;
  }

  .country {
    margin-right: 6px;
  }

  .kb {
    width: 100px;
    text-align: right;
  }

  .foot {
    margin-top: auto;
  }

  .remember {
    flex: 1;
  }

  .hide {
    min-width: 96px;
    margin-left: auto;
  }

  hr {
    width: 100%;
    margin: 2px 0;
    border: 0;
    border-top: 1px solid var(--fg-muted);
  }

  .note {
    font-size: 11px;
  }

  .then {
    width: 116px;
    margin-left: auto;
  }

  .indent {
    padding-left: 18px;
  }

  .controls {
    padding: 0 14px;
  }

  .btn.wide {
    width: 138px;
  }

  .btn.mid {
    width: 82px;
    margin-left: 24px;
  }

  .caption {
    text-align: center;
  }

  .strip {
    position: relative;
    height: 14px;
    background: var(--progress-track);
    border: 1px solid var(--border);
    overflow: hidden;
  }

  .reach {
    position: absolute;
    top: 3px;
    bottom: 3px;
    background: var(--segment);
  }

  .mark {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 2px;
    background: var(--error);
    z-index: 1;
  }

  .connections {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    background: var(--list);
    border: 1px solid var(--frame);
  }

  .head,
  .r {
    display: grid;
    grid-template-columns: 30px 104px 1fr;
  }

  .head {
    background: var(--header);
    height: 20px;
    align-items: center;
  }

  .head span {
    padding: 0 5px;
    border-right: 1px solid var(--grid);
  }

  .rows {
    overflow: auto;
  }

  .r span {
    padding: 0 5px;
    line-height: 16px;
  }
</style>
