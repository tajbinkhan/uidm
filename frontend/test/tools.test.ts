import { describe, expect, it } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import Links from '../src/windows/links/Links.svelte';
import Properties from '../src/windows/properties/Properties.svelte';
import Main from '../src/windows/main/Main.svelte';
import { sent, shell } from './helpers';

HTMLDialogElement.prototype.showModal ??= function (this: HTMLDialogElement) {
  this.open = true;
};

describe('the smaller tools', () => {
  it('the links window adds the ticked links to a queue', async () => {
    const calls = shell('links', 'links');
    render(Links);
    await fireEvent.click(screen.getByRole('checkbox', { name: 'photos.zip' }));
    await fireEvent.click(screen.getByRole('button', { name: 'OK' }));
    expect(sent(calls, 'add_links').at(-1)?.args).toEqual({ indices: [0, 2], queue: 1, start: false });
  });

  it('the properties window sends its fields', async () => {
    const calls = shell('properties-2', 'properties');
    render(Properties);
    await fireEvent.input(screen.getByLabelText('Description:'), { target: { value: 'installer' } });
    await fireEvent.click(screen.getByRole('button', { name: 'OK' }));
    expect(sent(calls, 'save_properties').at(-1)?.args).toMatchObject({ input: { description: 'installer' } });
  });

  it('Find selects the next match', async () => {
    const calls = shell('main', 'main');
    render(Main);
    await fireEvent.keyDown(window, { key: 'f', ctrlKey: true });
    await fireEvent.input(screen.getByLabelText('Find:'), { target: { value: 'usb-writer' } });
    await fireEvent.submit(screen.getByRole('dialog').querySelector('form') as HTMLFormElement);
    expect(sent(calls, 'find_download').at(-1)?.args).toMatchObject({ input: { text: 'usb-writer', name: true } });
  });

  it('the list menu moves downloads to a queue', async () => {
    const calls = shell('main', 'main');
    render(Main);
    await fireEvent.contextMenu(screen.getByRole('grid'), { clientX: 200, clientY: 120 });
    await fireEvent.click(screen.getByRole('menuitem', { name: 'Add to queue' }));
    await fireEvent.click(screen.getByRole('menuitem', { name: 'Synchronization queue' }));
    expect(sent(calls, 'queue_selection').at(-1)?.args).toEqual({ queue: 2 });
  });
});
