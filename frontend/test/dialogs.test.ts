import { describe, expect, it } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import AddUrl from '../src/windows/add-url/AddUrl.svelte';
import SaveAs from '../src/windows/save-as/SaveAs.svelte';
import Download from '../src/windows/download/Download.svelte';
import Done from '../src/windows/done/Done.svelte';
import Remove from '../src/windows/remove/Remove.svelte';
import { mockIPC } from '@tauri-apps/api/mocks';
import { sent, shell } from './helpers';

describe('the dialogs', () => {
  it('Add URL sends the address and its login', async () => {
    const calls = shell('add-url', 'add-url');
    render(AddUrl);
    await fireEvent.input(screen.getByLabelText('Address'), { target: { value: 'https://example.com/a.zip' } });
    await fireEvent.click(screen.getByLabelText('Use authorization'));
    await fireEvent.input(screen.getByLabelText('Login'), { target: { value: 'me' } });
    await fireEvent.input(screen.getByLabelText('Password'), { target: { value: 'pw' } });
    await fireEvent.click(screen.getByRole('button', { name: 'OK' }));
    expect(sent(calls, 'add_download').at(-1)?.args).toEqual({
      address: 'https://example.com/a.zip',
      login: { user: 'me', password: 'pw' },
    });
  });

  it('Download File Info answers with the category and the file', async () => {
    const calls = shell('save-as-17', 'save-as');
    render(SaveAs);
    expect((screen.getByLabelText('Save As') as HTMLInputElement).value).toBe('/home/u/Downloads/summer-photos.zip');
    await fireEvent.click(screen.getByRole('button', { name: 'Download Later' }));
    expect(sent(calls, 'answer_download').at(-1)?.args).toEqual({
      answer: {
        category: 'Compressed',
        saveAs: '/home/u/Downloads/summer-photos.zip',
        rememberFolder: false,
        description: '',
        start: false,
        duplicate: null,
        rememberDuplicate: false,
        elevate: false,
        folderOnly: false,
      },
    });
  });

  it('Download File Info offers administrator rights for a folder UDM may not write to', async () => {
    const calls = shell('save-as-17', 'save-as');
    mockIPC((cmd, args) => {
      calls.push({ cmd, args: (args ?? {}) as Record<string, unknown> });
      return cmd === 'check_save_folder' ? { denied: true } : null;
    });
    render(SaveAs);
    expect(await screen.findByText('UDM may not save files in this folder. Saving there needs administrator rights.')).toBeTruthy();
    expect(sent(calls, 'check_save_folder').at(-1)?.args).toEqual({ saveAs: '/home/u/Downloads/summer-photos.zip', folderOnly: false });
    const start = screen.getByRole('button', { name: 'Start Download' }) as HTMLButtonElement;
    expect(start.disabled).toBe(true);
    await fireEvent.click(screen.getByLabelText('Save there with administrator rights'));
    expect(start.disabled).toBe(false);
    await fireEvent.click(start);
    expect(sent(calls, 'answer_download').at(-1)?.args).toMatchObject({ answer: { start: true, elevate: true } });
  });

  it('the progress window shows the download and pauses it', async () => {
    const calls = shell('download-12', 'download');
    render(Download);
    expect(screen.getByText('633.453 MB ( 16.28 % )')).toBeTruthy();
    // The server's country, its flag made by the shell.
    expect(screen.getByTitle('Server country').textContent).toBe('\u{1F1FA}\u{1F1F8} US');
    await fireEvent.click(screen.getByRole('button', { name: 'Pause' }));
    expect(sent(calls, 'control_download').at(-1)?.args).toEqual({ action: 'pause' });
    await fireEvent.click(screen.getByRole('tab', { name: 'Speed Limiter' }));
    await fireEvent.click(screen.getByLabelText('Use Speed Limiter'));
    expect(sent(calls, 'set_download_limit').at(-1)?.args).toEqual({ limit: { on: true, kbPerSec: 10, remember: true } });
  });

  it('Download complete opens the file', async () => {
    const calls = shell('done-17', 'done');
    render(Done);
    expect(screen.getByText('Downloaded 40.00 MB (41943040 Bytes)')).toBeTruthy();
    await fireEvent.click(screen.getByRole('button', { name: 'Open' }));
    expect(sent(calls, 'open_download').at(-1)?.args).toEqual({ id: 17, how: 'file' });
  });

  it('Confirm deletion removes with the boxes ticked', async () => {
    const calls = shell('remove', 'remove');
    render(Remove);
    await fireEvent.click(screen.getByLabelText(/Delete completely downloaded files/));
    await fireEvent.click(screen.getByRole('button', { name: 'Yes' }));
    expect(sent(calls, 'remove_downloads').at(-1)?.args).toEqual({ deleteFiles: true, dontAsk: false });
  });
});
