import { describe, expect, it } from 'vitest';
import { fireEvent, render, screen, within } from '@testing-library/svelte';
import SaveAs from '../src/windows/save-as/SaveAs.svelte';
import Download from '../src/windows/download/Download.svelte';
import Options from '../src/windows/options/Options.svelte';
import Main from '../src/windows/main/Main.svelte';
import { editing } from '../src/windows/options/draft.svelte';
import { projection, receive } from '../src/stores/projection.svelte';
import type { DownloadView, SaveAsView, Update } from '../src/ipc/types';
import { fixtures, sent, shell } from './helpers';

/** Feed the window a changed copy of fixture `kind`. */
function changed<T>(kind: 'save-as-torrent' | 'download-torrent', change: (view: T) => void) {
  const fixture = structuredClone(fixtures.views[kind]) as unknown as { view: T };
  change(fixture.view);
  receive({ kind: 'snapshot', seq: 0, look: fixtures.look.dark, view: fixture } as unknown as Update);
}

describe("a torrent's Download File Info", () => {
  it('lists the files to tick and sends each tick to the shell', async () => {
    const calls = shell('save-as-20', 'save-as-torrent');
    render(SaveAs);
    expect(screen.getByRole('region', { name: 'Files in the torrent' })).toBeTruthy();
    expect(screen.getByText('4 of 5 files, 120.00 MB of 121.00 MB')).toBeTruthy();
    const cover = screen.getByLabelText('cover.jpg') as HTMLInputElement;
    expect(cover.checked).toBe(false);
    await fireEvent.click(screen.getByLabelText('01 - Opening.flac'));
    expect(sent(calls, 'pick_torrent_files').at(-1)?.args).toEqual({ indexes: [0], chosen: false });
    await fireEvent.click(screen.getByRole('button', { name: 'Select all' }));
    expect(sent(calls, 'pick_torrent_files').at(-1)?.args).toEqual({ indexes: [0, 1, 2, 3, 4], chosen: true });
    await fireEvent.click(screen.getByRole('button', { name: 'Select none' }));
    expect(sent(calls, 'pick_torrent_files').at(-1)?.args).toMatchObject({ chosen: false });
    expect(screen.getAllByRole('columnheader').map((h) => h.textContent)).toEqual(['File', 'Size']);
    await fireEvent.click(screen.getByRole('button', { name: 'Start Download' }));
    expect(sent(calls, 'answer_download').at(-1)?.args).toMatchObject({
      answer: { saveAs: '/home/u/Downloads/Free Music Album', folderOnly: false, start: true },
    });
  });

  it('cannot start with no file ticked', () => {
    shell('save-as-20', 'save-as-torrent');
    changed<SaveAsView>('save-as-torrent', (v) => {
      v.torrent!.noneChosen = true;
      for (const f of v.torrent!.files) f.chosen = false;
    });
    render(SaveAs);
    expect((screen.getByRole('button', { name: 'Start Download' }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole('button', { name: 'Download Later' }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('keeps the name of a torrent of one file and asks only its folder', async () => {
    const calls = shell('save-as-20', 'save-as-torrent');
    changed<SaveAsView>('save-as-torrent', (v) => {
      v.saveAs = '/home/u/Downloads/a.iso';
      v.torrent!.fixedName = 'a.iso';
      v.torrent!.files = [{ index: 0, path: 'a.iso', size: '1.00 GB', chosen: true, progress: '', fraction: null }];
    });
    render(SaveAs);
    const folder = (await screen.findByLabelText('Save to')) as HTMLInputElement;
    expect(folder.value).toBe('/home/u/Downloads');
    expect(screen.getByText('a.iso', { selector: '.fixed' })).toBeTruthy();
    await fireEvent.input(folder, { target: { value: '/srv/iso' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Start Download' }));
    expect(sent(calls, 'answer_download').at(-1)?.args).toMatchObject({ answer: { saveAs: '/srv/iso', folderOnly: true } });
  });

  it('waits for a magnet link’s file list, and can start with every file', () => {
    shell('save-as-20', 'save-as-torrent');
    changed<SaveAsView>('save-as-torrent', (v) => {
      v.torrent = { ...v.torrent!, waiting: true, files: [], chosen: 'Getting the file list from peers... Starting now downloads every file.' };
    });
    render(SaveAs);
    expect(screen.getByRole('status', { name: '' }).textContent).toContain('Getting the file list from peers');
    expect((screen.getByRole('button', { name: 'Start Download' }) as HTMLButtonElement).disabled).toBe(false);
  });
});

describe("a torrent's download window", () => {
  it('has tabs for the files, peers, trackers, pieces and seeding', async () => {
    const calls = shell('download-18', 'download-torrent');
    render(Download);
    expect(screen.getByText('Share ratio')).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Show details >>' })).toBeNull();
    const tabs = screen.getAllByRole('tab').map((t) => t.textContent?.trim());
    expect(tabs).toEqual(expect.arrayContaining(['Download status', 'Files', 'Peers', 'Trackers', 'Pieces', 'Seeding']));

    await fireEvent.click(screen.getByRole('tab', { name: 'Files' }));
    // Each file's bar, and the window's own.
    expect(screen.getAllByRole('progressbar').length).toBe(5);
    await fireEvent.click(screen.getByLabelText('README.txt'));
    expect(sent(calls, 'pick_torrent_files').at(-1)?.args).toEqual({ indexes: [4], chosen: false });

    await fireEvent.click(screen.getByRole('tab', { name: 'Peers' }));
    const peers = screen.getByRole('table');
    expect(within(peers).getAllByRole('columnheader').length).toBe(7);
    expect(within(peers).getByText('Transmission 4.0.6')).toBeTruthy();

    await fireEvent.click(screen.getByRole('tab', { name: 'Trackers' }));
    expect(screen.getByText('Skipped')).toBeTruthy();

    await fireEvent.click(screen.getByRole('tab', { name: 'Pieces' }));
    const map = screen.getByRole('img');
    expect(map.getAttribute('aria-label')).toContain('419 of 876 pieces');
    expect(map.children.length).toBe(512);
  });

  it('sets its own seeding limits and stops seeding', async () => {
    const calls = shell('download-18', 'download-torrent');
    changed<DownloadView>('download-torrent', (v) => {
      v.torrent!.seeding = true;
    });
    render(Download);
    await fireEvent.click(screen.getByRole('tab', { name: 'Seeding' }));
    expect(screen.getByText(/Until a share ratio of 1.00 or 60 minutes/)).toBeTruthy();
    await fireEvent.click(screen.getByLabelText('Use its own limits'));
    await fireEvent.click(screen.getAllByLabelText('Stop after this many minutes')[0]);
    await fireEvent.input(screen.getAllByLabelText('Stop at a share ratio of')[1], { target: { value: '2.5' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Apply' }));
    expect(sent(calls, 'set_seed_limits').at(-1)?.args).toEqual({
      limits: { own: true, ratioOn: true, ratio: 2.5, minutesOn: false, minutes: 60 },
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Stop seeding' }));
    expect(sent(calls, 'stop_seeding').length).toBe(1);
  });

  it('takes patches that change only the rows that changed', () => {
    shell('download-18', 'download-torrent');
    const view = fixtures.views['download-torrent'].view as unknown as DownloadView;
    const peer = { ...view.torrent!.peers[0], downSpeed: '9.0 MB/s' };
    receive({
      kind: 'patch',
      seq: 1,
      look: null,
      patch: { window: 'download', patch: { view: null, torrent: { files: [], fileOrder: null, peers: [peer], peerOrder: null, trackers: null, pieces: null } } },
    } as unknown as Update);
    const got = projection.view?.window === 'download' ? projection.view.view : null;
    expect(got?.torrent?.peers.map((p) => p.downSpeed)).toEqual(['9.0 MB/s', ...view.torrent!.peers.slice(1).map((p) => p.downSpeed)]);
    expect(got?.torrent?.files.length).toBe(view.torrent!.files.length);
    // A new view keeps the lists the window has.
    receive({
      kind: 'patch',
      seq: 2,
      look: null,
      patch: { window: 'download', patch: { view: { ...view, status: 'Seeding', torrent: { ...view.torrent!, files: [], peers: [], trackers: [], pieces: [] } }, torrent: null } },
    } as unknown as Update);
    const next = projection.view?.window === 'download' ? projection.view.view : null;
    expect(next?.status).toBe('Seeding');
    expect(next?.torrent?.peers[0].downSpeed).toBe('9.0 MB/s');
    expect(next?.torrent?.pieces.length).toBe(512);
  });
});

describe('torrents elsewhere', () => {
  it('Options > Torrents reaches the draft', async () => {
    editing.draft = null;
    const calls = shell('options', 'options');
    render(Options);
    await fireEvent.click(screen.getByRole('tab', { name: 'Torrents' }));
    await fireEvent.click(screen.getAllByLabelText('Listen on port')[0]);
    await fireEvent.input(screen.getAllByLabelText('Listen on port')[1], { target: { value: '51413' } });
    await fireEvent.click(screen.getByLabelText('Find peers through the DHT, without a tracker'));
    await fireEvent.click(screen.getAllByLabelText('Stop after this many minutes')[0]);
    await fireEvent.click(screen.getByRole('button', { name: 'OK' }));
    const extras = (sent(calls, 'save_options').at(-1)?.args.draft as { extras: Record<string, unknown> }).extras;
    expect(extras).toMatchObject({ torrentPortFixed: true, torrentPort: 51413, torrentDht: false, seedMinutesOn: false });
  });

  it('the main window opens torrent files and stops seeding', async () => {
    const calls = shell('main', 'main');
    const view = structuredClone(fixtures.views.main) as { view: { chrome: Record<string, unknown> } };
    view.view.chrome.selected = [19];
    view.view.chrome.can = { ...(view.view.chrome.can as object), torrent: true, stopSeeding: true };
    receive({ kind: 'snapshot', seq: 0, look: fixtures.look.dark, view } as Update);
    render(Main);
    await fireEvent.click(screen.getByRole('menuitem', { name: 'Tasks' }));
    await fireEvent.click(screen.getByRole('menuitem', { name: /Open torrent file/ }));
    expect(sent(calls, 'open_torrent_file').length).toBe(1);
    await fireEvent.contextMenu(screen.getByRole('grid'), { clientX: 200, clientY: 120 });
    await fireEvent.click(screen.getByRole('menuitem', { name: 'Stop seeding' }));
    expect(sent(calls, 'run_list_action').at(-1)?.args).toEqual({ action: { action: 'stopSeeding' } });
    await fireEvent.contextMenu(screen.getByRole('grid'), { clientX: 200, clientY: 120 });
    await fireEvent.click(screen.getByRole('menuitem', { name: 'Torrent details' }));
    expect(sent(calls, 'show_download').at(-1)?.args).toEqual({ id: 19 });
    expect(fixtures.views.main.view.rows.find((r) => r.id === 19)?.status).toBe('Seeding (ratio 0.41)');
  });
});
