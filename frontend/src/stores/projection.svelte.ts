// The window's projection store (AGENTS.md section 7.4): the look and the
// view the shell sends. Only the subscription writes it, through `receive`;
// components read it.
import type { DownloadPatch, DownloadView, Look, MainPatch, MainView, SaveAsPatch, SaveAsView, Update, View } from '../ipc/types';

class Projection {
  look = $state<Look | null>(null);
  view = $state<View | null>(null);
  /** The window's subject is gone; the shell is closing the window. */
  gone = $state(false);
}

const store = new Projection();

/** What the window draws. Read only. */
export const projection: Readonly<Projection> = store;

function patchMain(view: MainView, patch: MainPatch): MainView {
  // A lookup made and dropped within this call, never drawn from.
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  const rows = new Map(view.rows.map((r) => [r.id, r]));
  for (const r of patch.rows) rows.set(r.id, r);
  const order = patch.order ?? view.rows.map((r) => r.id);
  return {
    rows: order.flatMap((id) => {
      const r = rows.get(id);
      return r ? [r] : [];
    }),
    chrome: patch.chrome ?? view.chrome,
  };
}

/** `rows` with the rows that changed put in, in `order` when it changed. */
function patchRows<T, K>(rows: T[], changed: T[], order: K[] | null | undefined, key: (r: T) => K): T[] {
  if (changed.length === 0 && !order) return rows;
  // A lookup made and dropped within this call, never drawn from.
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
  const byKey = new Map(rows.map((r) => [key(r), r]));
  for (const r of changed) byKey.set(key(r), r);
  return (order ?? rows.map(key)).flatMap((k) => {
    const r = byKey.get(k);
    return r ? [r] : [];
  });
}

function patchDownload(view: DownloadView, patch: DownloadPatch): DownloadView {
  const next = patch.view ?? view;
  if (!next.torrent) return next;
  // A new view comes without its torrent's lists: the window keeps its own.
  const lists = view.torrent ?? { files: [], peers: [], trackers: [], pieces: [] };
  const own = patch.view ? lists : next.torrent;
  const p = patch.torrent;
  return {
    ...next,
    torrent: {
      ...next.torrent,
      files: patchRows(own.files, p?.files ?? [], p?.fileOrder, (r) => r.index),
      peers: patchRows(own.peers, p?.peers ?? [], p?.peerOrder, (r) => r.address),
      trackers: p?.trackers ?? own.trackers,
      pieces: p?.pieces ?? own.pieces,
    },
  };
}

function patchSaveAs(view: SaveAsView, patch: SaveAsPatch): SaveAsView {
  const next = patch.view ?? view;
  if (!next.torrent) return next;
  const own = patch.view ? (view.torrent?.files ?? []) : next.torrent.files;
  return { ...next, torrent: { ...next.torrent, files: patchRows(own, patch.files, patch.fileOrder, (r) => r.index) } };
}

/** Take one update from the subscription. */
export function receive(update: Update): void {
  switch (update.kind) {
    case 'snapshot':
      store.look = update.look;
      store.view = update.view;
      break;
    case 'patch': {
      if (update.look) store.look = update.look;
      const { patch } = update;
      if (patch.window === 'whole') {
        store.view = patch.patch;
      } else if (patch.window === 'main' && store.view?.window === 'main') {
        store.view = { window: 'main', view: patchMain(store.view.view, patch.patch) };
      } else if (patch.window === 'download' && store.view?.window === 'download') {
        store.view = { window: 'download', view: patchDownload(store.view.view, patch.patch) };
      } else if (patch.window === 'saveAs' && store.view?.window === 'saveAs') {
        store.view = { window: 'saveAs', view: patchSaveAs(store.view.view, patch.patch) };
      }
      break;
    }
    case 'gone':
      store.gone = true;
      break;
  }
}
