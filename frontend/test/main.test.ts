import { describe, expect, it } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import Main from '../src/windows/main/Main.svelte';
import { fixtures, sent, shell } from './helpers';
import { receive } from '../src/stores/projection.svelte';
import type { Update } from '../src/ipc/types';

describe('the main window', () => {
  it('draws the rows, the tree and the toolbar the snapshot holds', () => {
    shell('main', 'main');
    render(Main);
    // The table draws the rows in view, and says how many there are.
    const rows = screen.getAllByRole('row').filter((r) => r.getAttribute('aria-rowindex'));
    const all = fixtures.views.main.view.rows;
    expect(rows.length).toBe(Math.min(all.length, 17));
    expect(screen.getByRole('grid').getAttribute('aria-rowcount')).toBe(String(all.length));
    expect(screen.getByText('interview-notes.pdf')).toBeTruthy();
    expect(screen.getByRole('treeitem', { name: /All Downloads/ }).getAttribute('aria-selected')).toBe('true');
    expect((screen.getByRole('button', { name: /Stop All/ }) as HTMLButtonElement).disabled).toBe(false);
    expect((screen.getByRole('button', { name: /Resume$/ }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('sends what the user clicks', async () => {
    const calls = shell('main', 'main');
    render(Main);
    await fireEvent.click(screen.getByRole('button', { name: /Stop All/ }));
    expect(sent(calls, 'run_list_action').at(-1)?.args).toEqual({ action: { action: 'stopAll' } });
    await fireEvent.click(screen.getByRole('button', { name: /Add URL/ }));
    expect(sent(calls, 'open_window').at(-1)?.args).toEqual({ window: 'addUrl' });
    await fireEvent.mouseDown(screen.getByText('usb-writer-4.15.exe'), { button: 0 });
    expect(sent(calls, 'select_rows').at(-1)?.args).toEqual({ ids: [15] });
    await fireEvent.click(screen.getByRole('button', { name: 'Size' }));
    expect(sent(calls, 'sort_list').at(-1)?.args).toEqual({ column: 'size' });
    await fireEvent.click(screen.getByRole('treeitem', { name: /Finished/ }));
    expect(sent(calls, 'select_node').at(-1)?.args).toEqual({ key: 'finished' });
  });

  it('opens the list menu on a right click', async () => {
    shell('main', 'main');
    render(Main);
    await fireEvent.contextMenu(screen.getByRole('grid'), { clientX: 200, clientY: 120 });
    expect(screen.getByRole('menuitem', { name: 'Redownload' }).getAttribute('aria-disabled')).toBe('false');
    expect(screen.getByRole('menuitem', { name: 'Open with...' }).getAttribute('aria-disabled')).toBe('true');
  });

  it('opens the menu bar menus', async () => {
    const calls = shell('main', 'main');
    render(Main);
    await fireEvent.click(screen.getByRole('menuitem', { name: 'Help' }));
    await fireEvent.click(screen.getByRole('menuitem', { name: 'About UDM' }));
    expect(sent(calls, 'open_window').at(-1)?.args).toEqual({ window: 'about' });
  });

  it('says so when the list is empty, and only then', async () => {
    shell('main', 'main');
    render(Main);
    expect(screen.queryByText('No downloads here')).toBeNull();
    const view = structuredClone(fixtures.views.main) as { view: { rows: unknown[] } };
    view.view.rows = [];
    receive({ kind: 'snapshot', seq: 0, look: fixtures.look.light, view } as Update);
    await Promise.resolve();
    expect(screen.getByText('No downloads here')).toBeTruthy();
    expect(screen.getByText('Downloads started here or in the browser appear in this list.')).toBeTruthy();
  });

  it('opens a menu under its own name when the mouse slides to it', async () => {
    shell('main', 'main');
    render(Main);
    const tasks = screen.getByRole('menuitem', { name: 'Tasks' });
    const help = screen.getByRole('menuitem', { name: 'Help' });
    tasks.getBoundingClientRect = () => new DOMRect(6, 3, 40, 22);
    help.getBoundingClientRect = () => new DOMRect(215, 3, 43, 22);
    await fireEvent.click(tasks);
    await fireEvent.mouseEnter(help);
    await new Promise((r) => setTimeout(r));
    const menu = screen.getByRole('menuitem', { name: 'About UDM' }).closest<HTMLElement>('[role="menu"]');
    expect(menu?.style.left).toBe('215px');
  });
});

describe('the main window beyond IDM', () => {
  function withChrome(change: (chrome: Record<string, unknown>) => void) {
    const view = structuredClone(fixtures.views.main) as { view: { chrome: Record<string, unknown> } };
    change(view.view.chrome);
    receive({ kind: 'snapshot', seq: 0, look: fixtures.look.dark, view } as Update);
  }

  it('offers a problem report and acts on its buttons', async () => {
    const calls = shell('main', 'main');
    withChrome((c) => {
      c.report = { text: 'The UDM daemon has not answered for 30 seconds.', tone: 'warn', more: '', save: true, restart: true, issue: false, busy: false };
    });
    render(Main);
    await fireEvent.click(screen.getByRole('button', { name: 'Save a report' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Restart daemon' }));
    expect(sent(calls, 'report_action').map((c) => c.args)).toEqual([{ action: 'save' }, { action: 'restartDaemon' }]);
  });

  it('checks a finished file against a checksum', async () => {
    const calls = shell('main', 'main');
    const done = fixtures.views.main.view.rows.find((r) => r.finished)!.id;
    withChrome((c) => {
      c.selected = [done];
      (c.can as Record<string, boolean>).verify = true;
      c.checksum = 'sha256:abc';
    });
    render(Main);
    await fireEvent.contextMenu(screen.getByRole('grid'), { clientX: 200, clientY: 120 });
    await fireEvent.click(screen.getByRole('menuitem', { name: /Verify checksum/ }));
    expect((screen.getByLabelText(/Paste the MD5/) as HTMLInputElement).value).toBe('sha256:abc');
    await fireEvent.click(screen.getByRole('button', { name: 'Verify' }));
    expect(sent(calls, 'verify_checksum').at(-1)?.args).toEqual({ id: done, checksum: 'sha256:abc' });
  });
});
