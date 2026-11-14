import { describe, expect, it } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import Options from '../src/windows/options/Options.svelte';
import { editing } from '../src/windows/options/draft.svelte';
import { receive } from '../src/stores/projection.svelte';
import type { Update } from '../src/ipc/types';
import { fixtures, sent, shell } from './helpers';

// jsdom has no modal dialogs.
HTMLDialogElement.prototype.showModal ??= function (this: HTMLDialogElement) {
  this.open = true;
};

describe('the Options window', () => {
  it('saves the draft with what the user changed', async () => {
    editing.draft = null;
    const calls = shell('options', 'options');
    render(Options);
    await fireEvent.change(screen.getByLabelText('Theme'), { target: { value: 'dark' } });
    await fireEvent.click(screen.getByRole('tab', { name: 'Connection' }));
    await fireEvent.click(screen.getByLabelText('Download limits'));
    await fireEvent.click(screen.getByRole('button', { name: 'OK' }));
    const draft = sent(calls, 'save_options').at(-1)?.args.draft as Record<string, unknown>;
    expect(draft.theme).toBe('dark');
    expect(draft.quotaEnabled).toBe(true);
  });

  it('adds a site login from its sheet', async () => {
    editing.draft = null;
    const calls = shell('options', 'options');
    render(Options);
    await fireEvent.click(screen.getByRole('tab', { name: 'Sites Logins' }));
    await fireEvent.click(screen.getByRole('button', { name: 'New' }));
    await fireEvent.input(screen.getByLabelText('Server/path'), { target: { value: 'files.example.com' } });
    await fireEvent.input(screen.getByLabelText('User'), { target: { value: 'me' } });
    await fireEvent.input(screen.getByLabelText('Password'), { target: { value: 'pw' } });
    const sheet = screen.getByRole('dialog', { name: 'Site login' });
    await fireEvent.submit(sheet.querySelector('form') as HTMLFormElement);
    expect(screen.getByText('files.example.com')).toBeTruthy();
    await fireEvent.click(screen.getAllByRole('button', { name: 'OK' }).at(-1) as HTMLElement);
    const draft = sent(calls, 'save_options').at(-1)?.args.draft as { logins: unknown[] };
    expect(draft.logins).toContainEqual({ site: 'files.example.com', user: 'me', password: 'pw', stored: false });
  });
});

describe("Options' own pages", () => {
  it('leaves off what this system cannot do', async () => {
    shell('options', 'options');
    const view = { ...fixtures.views.options.view, creationTime: false, portable: true };
    receive({ kind: 'snapshot', seq: 1, look: fixtures.look.dark, view: { ...fixtures.views.options, view } } as Update);
    render(Options);
    expect((screen.getByLabelText('Launch UDM on startup') as HTMLInputElement).disabled).toBe(true);
    await fireEvent.click(screen.getByRole('tab', { name: 'Behavior' }));
    const created = screen.getByLabelText("Use the server's date for the file's creation time too") as HTMLInputElement;
    expect(created.disabled).toBe(true);
    expect(created.closest('label')?.getAttribute('title')).toBe('This system keeps no creation time that can be set; Windows and macOS do.');
  });

  it('the speed and behavior pages reach the draft', async () => {
    const calls = shell('options', 'options');
    render(Options);
    await fireEvent.click(screen.getByRole('tab', { name: 'Speed' }));
    await fireEvent.input(screen.getByLabelText('Downloads at once'), { target: { value: '5' } });
    await fireEvent.click(screen.getByRole('tab', { name: 'Behavior' }));
    await fireEvent.click(screen.getByLabelText('Show nothing'));
    await fireEvent.click(screen.getByRole('button', { name: 'OK' }));
    const draft = sent(calls, 'save_options').at(-1)?.args.draft as { doneDialog: boolean; extras: { maxConcurrent: number; notify: string } };
    expect(draft.extras.maxConcurrent).toBe(5);
    expect(draft.extras.notify).toBe('off');
    expect(draft.doneDialog).toBe(false);
  });

  it('the tools install, the browsers register and a phone pairs', async () => {
    const calls = shell('options', 'options');
    render(Options);
    await fireEvent.click(screen.getByRole('tab', { name: 'Tools' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Install gallery-dl' }));
    expect(sent(calls, 'install_tool').at(-1)?.args).toEqual({ tool: 'galleryDl' });
    await fireEvent.click(screen.getByRole('tab', { name: 'Browser' }));
    await fireEvent.click(screen.getByLabelText('Firefox'));
    expect(sent(calls, 'set_browser').at(-1)?.args).toEqual({ index: 1, on: true });
    await fireEvent.click(screen.getByRole('tab', { name: 'Remote' }));
    expect(screen.getByRole('img', { name: 'QR code for pairing a phone' })).toBeTruthy();
    await fireEvent.click(screen.getByRole('button', { name: 'Remove' }));
    expect(sent(calls, 'remove_device').at(-1)?.args).toEqual({ id: 1 });
    await fireEvent.click(screen.getByRole('button', { name: 'Done' }));
    expect(sent(calls, 'pair_phone').at(-1)?.args).toEqual({ on: false });
  });

  it('the debrid page shows the account without its key, checks it and saves a new key', async () => {
    editing.draft = null;
    const calls = shell('options', 'options');
    render(Options);
    await fireEvent.click(screen.getByRole('tab', { name: 'Debrid' }));
    expect((screen.getByLabelText("Download file hosts' links through a debrid account") as HTMLInputElement).checked).toBe(true);
    expect((screen.getByLabelText('Real-Debrid') as HTMLInputElement).checked).toBe(true);
    const key = screen.getByLabelText('API key') as HTMLInputElement;
    expect(key.value).toBe('');
    expect(key.type).toBe('password');
    expect(screen.getByText('A key is stored.')).toBeTruthy();
    expect(screen.getByText(/^The key works: ann, premium until /)).toBeTruthy();
    expect(screen.getByText('Find the key at https://real-debrid.com/apitoken')).toBeTruthy();
    await fireEvent.click(screen.getByRole('button', { name: 'Check account' }));
    expect(sent(calls, 'check_debrid').at(-1)?.args).toEqual({ service: 'realDebrid', key: '' });
    await fireEvent.click(screen.getByLabelText('AllDebrid'));
    // Real-Debrid's key is not AllDebrid's: checking needs a key typed.
    expect(screen.getByText('The stored key is for Real-Debrid. Type a key for this service.')).toBeTruthy();
    expect((screen.getByRole('button', { name: 'Check account' }) as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.input(key, { target: { value: 'NEWKEY' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Check account' }));
    expect(sent(calls, 'check_debrid').at(-1)?.args).toEqual({ service: 'allDebrid', key: 'NEWKEY' });
    await fireEvent.click(screen.getByRole('button', { name: 'OK' }));
    const draft = sent(calls, 'save_options').at(-1)?.args.draft as { extras: Record<string, unknown> };
    expect(draft.extras).toMatchObject({ debridOn: true, debridService: 'allDebrid', debridKey: 'NEWKEY', debridForget: false });
  });

  it('the network page binds downloads to an adapter and picks the resolver', async () => {
    editing.draft = null;
    const calls = shell('options', 'options');
    render(Options);
    await fireEvent.click(screen.getByRole('tab', { name: 'Network' }));
    const adapter = screen.getByLabelText('Download through') as HTMLSelectElement;
    expect(adapter.value).toBe('wg0');
    expect([...adapter.options].map((o) => o.text)).toEqual(['Any network (the system chooses)', 'eth0 (192.168.1.5, fe80::5)', 'wg0 (10.2.0.2)']);
    expect((screen.getByLabelText('Quad9 (9.9.9.9)') as HTMLInputElement).checked).toBe(true);
    const url = screen.getByLabelText('Address') as HTMLInputElement;
    expect(url.disabled).toBe(true);
    await fireEvent.change(adapter, { target: { value: 'eth0' } });
    await fireEvent.click(screen.getByLabelText('Another DNS over HTTPS server'));
    expect(url.disabled).toBe(false);
    await fireEvent.input(url, { target: { value: 'https://doh.example/dns-query' } });
    await fireEvent.click(screen.getByRole('button', { name: 'OK' }));
    let draft = sent(calls, 'save_options').at(-1)?.args.draft as { extras: Record<string, unknown> };
    expect(draft.extras).toMatchObject({ adapter: { id: 'eth0', name: 'eth0' }, dns: 'custom', dnsUrl: 'https://doh.example/dns-query' });
    // The system's DNS, while bound, says lookups may leave by another adapter.
    await fireEvent.click(screen.getByLabelText("The system's (not encrypted)"));
    expect(screen.getByText(/^The system looks up host names itself/)).toBeTruthy();
    await fireEvent.change(adapter, { target: { value: '' } });
    expect(screen.queryByText(/^The system looks up host names itself/)).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'OK' }));
    draft = sent(calls, 'save_options').at(-1)?.args.draft as { extras: Record<string, unknown> };
    expect(draft.extras).toMatchObject({ adapter: null, dns: 'system' });
  });

  it('a stored debrid key can be removed, and checking needs a key', async () => {
    editing.draft = null;
    const calls = shell('options', 'options');
    render(Options);
    await fireEvent.click(screen.getByRole('tab', { name: 'Debrid' }));
    await fireEvent.click(screen.getByLabelText('Remove the stored key when saving'));
    expect(screen.getByText('No key is stored.')).toBeTruthy();
    expect((screen.getByRole('button', { name: 'Check account' }) as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.click(screen.getByRole('button', { name: 'OK' }));
    const draft = sent(calls, 'save_options').at(-1)?.args.draft as { extras: Record<string, unknown> };
    expect(draft.extras).toMatchObject({ debridKey: '', debridForget: true });
  });
});
