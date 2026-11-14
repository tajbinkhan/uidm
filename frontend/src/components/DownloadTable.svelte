<!-- The download list: the rows the shell sends, already filtered, sorted and
     formatted. A click on a header sorts by it, dragging its edge resizes it.
     Selection works as a desktop list's does (click, Ctrl, Shift, keys). -->
<script lang="ts">
  import type { ColumnHead, ColumnKey, Id, Row } from '../ipc/types';
  import { t } from '../i18n';
  import FileGlyph from './FileGlyph.svelte';

  let {
    rows,
    columns,
    selected,
    onselect,
    onsort,
    onresize,
    onopen,
    ondelete,
    onselectall,
    onmenu,
    ondrag,
    mono = false,
    size = 12,
  }: {
    rows: Row[];
    columns: ColumnHead[];
    selected: Id[];
    onselect: (ids: Id[]) => void;
    onsort: (key: ColumnKey) => void;
    onresize: (key: ColumnKey, width: number) => void;
    onopen: (row: Row) => void;
    ondelete: () => void;
    onselectall: () => void;
    onmenu: (x: number, y: number) => void;
    /** A finished row's file dragged out of the window, when the system can. */
    ondrag?: (row: Row) => void;
    /** The list's font: fixed-width or the window's, and its size. */
    mono?: boolean;
    size?: number;
  } = $props();

  // Rows grow with the list's font: 24 pixels for the normal 12, room for
  // the 16-pixel icons with air above and below.
  const ROW = $derived(Math.max(20, Math.round(size * 2)));

  let body: HTMLDivElement | undefined = $state();
  let scrollTop = $state(0);
  let scrollLeft = $state(0);
  let height = $state(300);
  let focus = $state(-1);
  let anchor = $state(-1);
  /** A column being dragged wider or narrower, before it is sent. */
  let dragging: { key: ColumnKey; width: number } | null = $state(null);

  const picked = $derived(new Set(selected));
  const widths = $derived(columns.map((c) => (dragging?.key === c.key ? dragging.width : (c.width ?? 80))));
  const total = $derived(widths.reduce((a, b) => a + b, 0));
  // Where each column ends, for the lines that part them down to the foot
  // of the list, past the last row.
  const edges = $derived(widths.map((_, i) => widths.slice(0, i + 1).reduce((a, b) => a + b, 0)));
  const first = $derived(Math.max(0, Math.floor(scrollTop / ROW) - 4));
  const last = $derived(Math.min(rows.length, Math.ceil((scrollTop + height) / ROW) + 4));
  const page = $derived(Math.max(1, Math.floor(height / ROW) - 1));

  $effect(() => {
    if (!body) return;
    const ro = new ResizeObserver(() => (height = body?.clientHeight ?? height));
    ro.observe(body);
    return () => ro.disconnect();
  });

  function cell(r: Row, key: ColumnKey): string {
    switch (key) {
      case 'name':
        return r.name;
      case 'queue':
        return '';
      case 'size':
        return r.size;
      case 'progress':
        return r.progress;
      case 'speed':
        return r.speed;
      case 'timeLeft':
        return r.timeLeft;
      case 'status':
        return r.status;
      case 'lastTry':
        return r.lastTry;
      case 'saveTo':
        return r.saveTo;
      case 'page':
        return r.page;
      case 'referrer':
        return r.referrer;
      case 'description':
        return r.description;
      case 'country':
        return r.country;
    }
  }

  function range(a: number, b: number): Id[] {
    const [lo, hi] = a < b ? [a, b] : [b, a];
    return rows.slice(lo, hi + 1).map((r) => r.id);
  }

  function reveal(i: number) {
    if (!body) return;
    const top = i * ROW;
    if (top < body.scrollTop) body.scrollTop = top;
    else if (top + ROW > body.scrollTop + body.clientHeight) body.scrollTop = top + ROW - body.clientHeight;
  }

  /** Hand a finished row's file to the shell to drag, once the pointer
   * moves a few pixels with the left button down. */
  function watchDrag(e: MouseEvent, row: Row) {
    if (!ondrag || !row.finished) return;
    const start = { x: e.clientX, y: e.clientY };
    const move = (ev: MouseEvent) => {
      if (Math.abs(ev.clientX - start.x) + Math.abs(ev.clientY - start.y) < 5) return;
      stop();
      if (ev.buttons & 1) ondrag(row);
    };
    const stop = () => {
      window.removeEventListener('mousemove', move);
      window.removeEventListener('mouseup', stop);
    };
    window.addEventListener('mousemove', move);
    window.addEventListener('mouseup', stop);
  }

  function click(e: MouseEvent, i: number) {
    body?.focus();
    const id = rows[i].id;
    if (e.shiftKey && anchor >= 0) {
      onselect(range(anchor, i));
    } else if (e.ctrlKey || e.metaKey) {
      onselect(picked.has(id) ? selected.filter((s) => s !== id) : [...selected, id]);
      anchor = i;
    } else {
      onselect([id]);
      anchor = i;
    }
    focus = i;
  }

  function onkeydown(e: KeyboardEvent) {
    if (rows.length === 0) return;
    const ctrl = e.ctrlKey || e.metaKey;
    const from = focus < 0 ? 0 : focus;
    const moves: Record<string, number> = {
      ArrowDown: from + 1,
      ArrowUp: from - 1,
      PageDown: from + page,
      PageUp: from - page,
      Home: 0,
      End: rows.length - 1,
    };
    if (e.key in moves) {
      e.preventDefault();
      const to = Math.max(0, Math.min(rows.length - 1, moves[e.key]));
      focus = to;
      reveal(to);
      if (ctrl) return;
      if (e.shiftKey) {
        if (anchor < 0) anchor = from;
        onselect(range(anchor, to));
      } else {
        anchor = to;
        onselect([rows[to].id]);
      }
      return;
    }
    const id = rows[from]?.id;
    if (e.key === ' ' && id !== undefined) {
      e.preventDefault();
      if (e.shiftKey && anchor >= 0) onselect(range(anchor, from));
      else if (ctrl) onselect(picked.has(id) ? selected.filter((s) => s !== id) : [...selected, id]);
      else onselect([id]);
    } else if (ctrl && e.key.toLowerCase() === 'a') {
      e.preventDefault();
      onselectall();
    } else if (e.key === 'Enter' && selected.length === 1) {
      e.preventDefault();
      const r = rows.find((x) => x.id === selected[0]);
      if (r) onopen(r);
    } else if (e.key === 'Delete') {
      e.preventDefault();
      ondelete();
    } else if ((e.key === 'F10' && e.shiftKey) || e.key === 'ContextMenu') {
      e.preventDefault();
      const r = body?.getBoundingClientRect();
      if (r) onmenu(r.left + 40, r.top + (from + 1) * ROW - (body?.scrollTop ?? 0));
    }
  }

  function startResize(e: PointerEvent, i: number) {
    e.preventDefault();
    e.stopPropagation();
    const key = columns[i].key;
    const start = e.clientX;
    const base = widths[i];
    const target = e.currentTarget as HTMLElement;
    target.setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) => (dragging = { key, width: Math.max(24, Math.min(800, base + ev.clientX - start)) });
    const up = () => {
      target.removeEventListener('pointermove', move);
      target.removeEventListener('pointerup', up);
      if (dragging) onresize(dragging.key, dragging.width);
      dragging = null;
    };
    target.addEventListener('pointermove', move);
    target.addEventListener('pointerup', up);
  }
</script>

<div class="table" class:mono style:--list-size="{size}px" style:--row="{ROW}px">
  <div class="headers" role="row" style:transform="translateX({-scrollLeft}px)">
    {#each columns as c, i (c.key)}
      <div
        class="th"
        role="columnheader"
        aria-sort={c.sort === 'ascending' ? 'ascending' : c.sort === 'descending' ? 'descending' : 'none'}
        style:width="{widths[i]}px"
      >
        <button
          class="title"
          disabled={!c.sortable}
          title={c.title}
          onclick={() => c.sortable && onsort(c.key)}
          aria-label={c.key === 'queue' ? t('Queue') : c.title}
        >
          <span class="ell">{c.title}</span>
          {#if c.sort}
            <svg class="arrow" width="9" height="10" viewBox="0 0 9 10" aria-hidden="true">
              <path d={c.sort === 'ascending' ? 'M4.5 9V1M1 4.5l3.5-3.5 3.5 3.5' : 'M4.5 1v8M1 5.5l3.5 3.5 3.5-3.5'} fill="none" stroke="currentColor" />
            </svg>
          {/if}
        </button>
        <span class="grip" role="presentation" onpointerdown={(e) => startResize(e, i)}></span>
      </div>
    {/each}
    <div class="th filler"></div>
  </div>
  <div
    class="body"
    bind:this={body}
    role="grid"
    aria-label={t('Downloads')}
    aria-rowcount={rows.length}
    aria-multiselectable="true"
    tabindex="0"
    onscroll={() => {
      scrollTop = body?.scrollTop ?? 0;
      scrollLeft = body?.scrollLeft ?? 0;
    }}
    {onkeydown}
    oncontextmenu={(e) => {
      e.preventDefault();
      onmenu(e.clientX, e.clientY);
    }}
  >
    <div class="sizer" style:height="{rows.length * ROW}px" style:width="{total}px">
      {#each rows.slice(first, last) as r, k (r.id)}
        {@const i = first + k}
        <div
          class="tr"
          class:selected={picked.has(r.id)}
          class:focused={i === focus}
          class:failed={r.tone === 'failed'}
          role="row"
          tabindex="-1"
          aria-rowindex={i + 1}
          aria-selected={picked.has(r.id)}
          style:top="{i * ROW}px"
          onmousedown={(e) => {
            if (e.button === 0) {
              click(e, i);
              watchDrag(e, r);
            }
            else if (e.button === 2 && !picked.has(r.id)) {
              onselect([r.id]);
              anchor = i;
              focus = i;
            }
          }}
          ondblclick={() => onopen(r)}
        >
          {#each columns as c, j (c.key)}
            <div class="td" class:right={c.key === 'size'} role="gridcell" style:width="{widths[j]}px">
              {#if c.key === 'name'}
                <FileGlyph kind={r.icon} />
                <span class="ell">{r.name}</span>
              {:else if c.key === 'queue'}
                {#if r.queued}
                  <svg width="14" height="14" viewBox="0 0 16 16" aria-label={t('Queued')} role="img">
                    <path d="M1.5 3.5h5l1.5 1.5h6.5v8.5h-13z" fill="var(--tint-folder)" stroke="var(--fg-muted)" />
                    <path d="M4.5 8h7M4.5 10h7" stroke="var(--fg)" />
                  </svg>
                {/if}
              {:else}
                <span class="ell">{cell(r, c.key)}</span>
              {/if}
            </div>
          {/each}
        </div>
      {/each}
      {#each edges as x, j (j)}
        <span class="guide" style:left="{x - 1}px"></span>
      {/each}
    </div>
  </div>
  {#if rows.length === 0}
    <div class="empty">
      <div class="note">
        <p class="empty-title">{t('No downloads here')}</p>
        <p>{t('Downloads started here or in the browser appear in this list.')}</p>
      </div>
    </div>
  {/if}
</div>

<style>
  .table {
    position: relative;
    display: flex;
    flex-direction: column;
    height: 100%;
    min-width: 0;
    background: var(--list);
    border: 1px solid var(--frame);
    border-radius: var(--radius-large);
    overflow: hidden;
  }

  .headers {
    display: flex;
    height: 26px;
    flex: none;
    background: var(--header);
    border-bottom: 1px solid var(--grid);
  }

  /* Columns part with a line that runs on down through the rows. */
  .th {
    position: relative;
    flex: none;
  }

  .th:not(.filler)::after {
    content: '';
    position: absolute;
    right: 0;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--grid);
    pointer-events: none;
  }

  .th.filler {
    flex: 1;
    min-width: 400px;
    border-right: 0;
  }

  .title {
    display: flex;
    align-items: center;
    width: 100%;
    height: 100%;
    padding: 0 6px;
    background: none;
    border: 0;
    text-align: left;
    color: var(--fg-muted);
  }

  .title:hover:not(:disabled) {
    background: var(--hover);
  }

  .title:disabled {
    color: var(--fg-muted);
  }

  .title .ell {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .arrow {
    flex: none;
    color: var(--fg-muted);
  }

  .grip {
    position: absolute;
    top: 0;
    right: -4px;
    width: 8px;
    height: 100%;
    cursor: col-resize;
    z-index: 1;
  }

  .body {
    position: relative;
    flex: 1;
    overflow: auto;
  }

  /* What an empty list says, in its middle; clicks pass through to the
     list, so its menu still opens. */
  .empty {
    position: absolute;
    inset: 26px 0 0;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0 24px;
    pointer-events: none;
  }

  /* On the list's own color, so the column lines stop at its edges. */
  .note {
    display: flex;
    flex-direction: column;
    gap: 4px;
    max-width: 340px;
    padding: 10px 16px;
    background: var(--list);
    color: var(--fg-muted);
    text-align: center;
  }

  .empty p {
    margin: 0;
  }

  .empty .empty-title {
    color: var(--fg);
    font-size: calc(var(--text) + 2px);
    font-weight: 600;
  }

  .sizer {
    position: relative;
    min-width: 100%;
    min-height: 100%;
  }

  /* The lines between columns, over the rows and down to the list's foot. */
  .guide {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--grid);
    pointer-events: none;
  }

  .tr {
    position: absolute;
    left: 0;
    display: flex;
    height: var(--row);
    font-size: var(--list-size);
    min-width: 100%;
  }

  .tr:hover:not(.selected) {
    background: var(--hover);
  }

  /* A selected row carries the accent's bar on its leading edge. */
  .tr.selected {
    background: var(--selected);
    color: var(--selected-fg);
    box-shadow: inset 3px 0 0 var(--accent);
  }

  .body:not(:focus) .tr.selected {
    background: var(--selected-blur);
    box-shadow: none;
  }

  .body:focus-visible .tr.focused {
    outline: 1px solid var(--focus-ring);
    outline-offset: -1px;
  }

  .tr.failed .td:not(:first-child) {
    color: var(--error);
  }

  .td {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: none;
    padding: 0 6px;
    overflow: hidden;

    white-space: nowrap;
    line-height: var(--row);
  }

  .mono .tr {
    font-family: ui-monospace, 'Cascadia Mono', Consolas, 'DejaVu Sans Mono', monospace;
  }

  .td.right {
    justify-content: flex-start;
  }

  .td .ell {
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
