<!-- Download File Info: the category and the file a download is saved as,
     before it starts now or waits in a queue, and for a torrent the files
     to download. Closing it cancels the download, as Cancel does. -->
<script lang="ts">
  import { answerDownload, checkSaveFolder, closeWindow, fitHeight, messageOf, pickSaveFolder, pickTorrentFiles, previewZip } from '../../ipc';
  import type { DuplicateChoice, SaveAsView } from '../../ipc/types';
  import { projection } from '../../stores/projection.svelte';
  import { t } from '../../i18n';
  import Window from '../../components/Window.svelte';
  import FileGlyph from '../../components/FileGlyph.svelte';
  import Sheet from '../../components/Sheet.svelte';

  const view: SaveAsView | null = $derived(projection.view?.window === 'saveAs' ? projection.view.view : null);

  /** The dialog's drafts, taken from the view when it first comes. */
  let draft = $state({ category: '', saveAs: '', remember: false, description: '' });
  let loaded = $state(false);
  let error = $state('');
  let busy = $state(false);
  /** The Duplicate download link question: open, the choice, and whether to
   * remember it. */
  let asking = $state(false);
  let duplicate = $state<DuplicateChoice | null>(null);
  let rememberDuplicate = $state(false);
  /** UDM may not write to the folder Save As names, as the shell last
   * checked it, and whether to save there with administrator rights. */
  let denied = $state(false);
  let elevate = $state(false);
  const blocked = $derived(denied && !elevate);
  /** The window's size (the shell's `windows.rs`, larger for a torrent's
   * file list), and how much the warning for such a folder adds. */
  const WIDTH = 576;
  const HEIGHT = $derived(view?.torrent ? 432 : 206);
  const WARNING = 40;
  /** A torrent of one file keeps its name: Save As then holds its folder,
   * which the shell knows to take as one. */
  let folderOnly = $state(false);
  const torrent = $derived(view?.torrent ?? null);

  // Check the folder a moment after the Save As box stops changing.
  $effect(() => {
    if (!loaded) return;
    const saveAs = draft.saveAs;
    const timer = setTimeout(() => {
      checkSaveFolder(saveAs)
        .then((access) => {
          if (draft.saveAs !== saveAs || access.denied === denied) return;
          denied = access.denied;
          void fitHeight(WIDTH, HEIGHT + (denied ? WARNING : 0)).catch(() => undefined);
        })
        .catch(() => undefined);
    }, 300);
    return () => clearTimeout(timer);
  });

  async function browse() {
    error = '';
    try {
      const picked = await pickSaveFolder(draft.saveAs);
      if (picked) draft.saveAs = picked;
    } catch (e) {
      error = messageOf(e);
    }
  }

  $effect(() => {
    if (view && !loaded) {
      draft = { category: view.category, saveAs: view.saveAs, remember: view.rememberFolder, description: view.description };
      loaded = true;
      if (view.duplicate) {
        duplicate = view.duplicate.otherRunning ? 'newName' : 'overwrite';
        asking = true;
      }
    }
  });

  // Once a torrent's files are known, one of a single file shows its folder.
  $effect(() => {
    if (loaded && !folderOnly && torrent?.fixedName) {
      folderOnly = true;
      draft.saveAs = torrent.folder;
    }
  });

  async function pick(indexes: number[], chosen: boolean) {
    error = '';
    try {
      await pickTorrentFiles(indexes, chosen);
    } catch (e) {
      error = messageOf(e);
    }
  }

  async function answer(start: boolean) {
    if (busy || blocked || torrent?.noneChosen) return;
    busy = true;
    error = '';
    try {
      await answerDownload({
        category: draft.category,
        saveAs: draft.saveAs,
        rememberFolder: draft.remember,
        description: draft.description,
        start,
        duplicate,
        rememberDuplicate,
        elevate: denied && elevate,
      });
    } catch (e) {
      error = messageOf(e);
    } finally {
      busy = false;
    }
  }
</script>

<Window onescape={() => void closeWindow()}>
  {#if view}
    <form
      class="dialog"
      onsubmit={(e) => {
        e.preventDefault();
        void answer(true);
      }}
    >
      <div class="grid">
        <label for="url">{t('URL')}</label>
        <input id="url" class="text" value={view.url} readonly spellcheck="false" />

        <label for="category">{t('Category')}</label>
        <div class="row">
          <select id="category" class="select category" bind:value={draft.category}>
            {#each view.categories as c (c)}<option value={c}>{t(c)}</option>{/each}
          </select>
          <button class="btn small" type="button" disabled aria-label={t('Add category')} title={t('Add category')}>+</button>
        </div>
        <div class="side big">
          <FileGlyph kind={view.icon} size={40} />
          <span>{view.size}</span>
          {#if view.canPreview}
            <button
              class="btn small"
              type="button"
              title={t('List the files inside, reading only the end of the archive')}
              onclick={() => view && void previewZip(view.id)}>{t('Preview')}</button
            >
          {/if}
        </div>

        <label for="save-as">{folderOnly ? t('Save to') : t('Save As')}</label>
        <div class="row">
          <input id="save-as" class="text grow" bind:value={draft.saveAs} list={folderOnly ? undefined : 'folders'} spellcheck="false" />
          <datalist id="folders">
            {#each view.folders as f (f)}<option value={f}></option>{/each}
          </datalist>
          <button class="btn small" type="button" aria-label={t('Browse...')} title={t('Browse...')} onclick={() => void browse()}>...</button>
        </div>

        <fieldset class="group">
          <legend>
            <label class="check">
              <input type="checkbox" bind:checked={draft.remember} />
              {t('Remember this path for "{category}" category', { category: t(draft.category) })}
            </label>
          </legend>
          <input class="text wide" value={view.remembered} readonly aria-label={t('Remembered folder')} />
        </fieldset>

        {#if folderOnly && torrent?.fixedName}
          <span class="label">{t('File name')}</span>
          <span class="fixed" title={t("A torrent of one file keeps its file's name; choose only its folder.")}>{torrent.fixedName}</span>
        {/if}

        <label for="description">{t('Description')}</label>
        <input id="description" class="text" bind:value={draft.description} />
      </div>
      {#if torrent}
        <section class="torrent" aria-label={t('Files in the torrent')}>
          {#if torrent.waiting}
            <p class="waiting" role="status">{torrent.chosen}</p>
          {:else}
            <div class="row">
              <span class="grow">{t('Files in the torrent')}</span>
              <button class="btn" type="button" onclick={() => void pick(torrent.files.map((f) => f.index), true)}>{t('Select all')}</button>
              <button class="btn" type="button" onclick={() => void pick(torrent.files.map((f) => f.index), false)}>{t('Select none')}</button>
            </div>
            <div class="files">
              <table>
                <thead>
                  <tr><th scope="col">{t('File')}</th><th scope="col" class="size">{t('Size')}</th></tr>
                </thead>
                <tbody>
                  {#each torrent.files as f (f.index)}
                    <tr>
                      <td class="path">
                        <label class="check" title={f.path}
                          ><input type="checkbox" checked={f.chosen} onchange={(e) => void pick([f.index], e.currentTarget.checked)} />{f.path}</label
                        >
                      </td>
                      <td class="size">{f.size}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
            <p class="chosen" class:error-text={torrent.noneChosen} role="status">{torrent.chosen}</p>
            {#if torrent.private}<p class="note">{t('A private torrent finds peers through its trackers only.')}</p>{/if}
          {/if}
        </section>
      {/if}
      {#if denied}
        <div class="denied">
          <p class="error-text" role="status">{t('UDM may not save files in this folder. Saving there needs administrator rights.')}</p>
          {#if view.canElevate}
            <label class="check" title={t('The download runs as you; Windows asks once it is done, before the file is moved there')}
              ><input type="checkbox" bind:checked={elevate} />{t('Save there with administrator rights')}</label
            >
          {/if}
        </div>
      {/if}
      {#if error}<p class="error-text" role="alert">{error}</p>{/if}
      <div class="buttons">
        <button class="btn" type="button" disabled={busy || blocked || torrent?.noneChosen} onclick={() => void answer(false)}>{t('Download Later')}</button>
        <button class="btn default" type="submit" disabled={busy || blocked || torrent?.noneChosen}>{t('Start Download')}</button>
        <button class="btn" type="button" onclick={() => void closeWindow()}>{t('Cancel')}</button>
      </div>
    </form>
    {#if asking && view.duplicate}
      <Sheet
        title={t('Duplicate download link')}
        width={420}
        oncancel={() => void closeWindow()}
        onok={() => (asking = false)}
      >
        <input class="text" readonly value={view.url} aria-label={t('Address')} />
        <p>{t('This file already exists in your download list. You may choose one of the following options, or press the Cancel button to skip the download of this file.')}</p>
        <label class="check"><input type="radio" name="dup" value="newName" bind:group={duplicate} />{t('Add the duplicate with a numbered file name')}</label>
        <label class="check"
          ><input type="radio" name="dup" value="overwrite" bind:group={duplicate} disabled={view.duplicate.otherRunning} />{t(
            'Add the duplicate and overwrite the existing file',
          )}</label
        >
        <label class="check"
          ><input type="radio" name="dup" value="existing" bind:group={duplicate} />{t(
            'If existing file is complete, show download complete dialog; otherwise resume it.',
          )}</label
        >
        <label class="check"
          ><input type="checkbox" bind:checked={rememberDuplicate} />{t(
            "Remember my selection and don't show this dialog again. You may change it in UDM Options > Downloads at a later time.",
          )}</label
        >
      </Sheet>
    {/if}
  {/if}
</Window>

<style>
  .dialog {
    gap: 6px;
    padding: 8px 10px 8px 12px;
  }

  .grid {
    display: grid;
    grid-template-columns: 78px 364px 1fr;
    align-items: center;
    gap: 6px 8px;
  }

  .grid > label {
    grid-column: 1;
    text-align: right;
  }

  .grid > :not(label):not(.big) {
    grid-column: 2;
  }

  .category {
    width: 172px;
  }

  .big {
    grid-column: 3;
    grid-row: 1 / span 4;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    padding-top: 6px;
  }

  fieldset {
    padding: 2px 8px 8px;
  }

  .wide {
    width: 100%;
  }

  .denied {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding-left: 86px;
  }

  .buttons {
    justify-content: flex-start;
    padding-left: 86px;
    gap: 22px;
  }

  .buttons .btn {
    min-width: 108px;
  }

  p {
    margin: 0;
  }

  .grid > .label {
    grid-column: 1;
    text-align: right;
  }

  .fixed {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .torrent {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding-left: 86px;
  }

  .files {
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
    background: var(--header);
    font-weight: normal;
    text-align: left;
    padding: 0 5px;
    border-right: 1px solid var(--grid);
  }

  td {
    padding: 0 5px;
    line-height: 18px;
  }

  .path {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .path label {
    display: inline;
  }

  .size {
    width: 84px;
    text-align: right;
    white-space: nowrap;
  }

  .waiting {
    padding: 12px 0;
  }

  .note {
    font-size: 11px;
  }

  input[type='radio'] {
    margin: 0 5px 0 0;
    accent-color: var(--focus);
  }
</style>
