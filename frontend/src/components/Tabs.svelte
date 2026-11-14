<!-- Tabs over a framed page, as a desktop property sheet draws them. Left
     and Right move between tabs. -->
<script lang="ts">
  import type { Snippet } from 'svelte';

  let {
    tabs,
    current = $bindable(),
    wrap = false,
    children,
  }: { tabs: { key: string; label: string }[]; current: string; wrap?: boolean; children: Snippet } = $props();

  let buttons: HTMLButtonElement[] = $state([]);

  function onkeydown(e: KeyboardEvent, i: number) {
    const step = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
    if (!step) return;
    e.preventDefault();
    const j = (i + step + tabs.length) % tabs.length;
    current = tabs[j].key;
    buttons[j]?.focus();
  }
</script>

<div class="tabs">
  <div class="strip" class:wrap role="tablist">
    {#each tabs as tab, i (tab.key)}
      <button
        bind:this={buttons[i]}
        class="tab"
        class:on={current === tab.key}
        role="tab"
        aria-selected={current === tab.key}
        tabindex={current === tab.key ? 0 : -1}
        onclick={() => (current = tab.key)}
        onkeydown={(e) => onkeydown(e, i)}
      >
        {tab.label}
      </button>
    {/each}
  </div>
  <div class="page" role="tabpanel">
    {@render children()}
  </div>
</div>

<style>
  .tabs {
    display: flex;
    flex-direction: column;
  }

  .strip {
    display: flex;
    align-items: stretch;
    gap: 2px;
    height: 24px;
    margin-bottom: 4px;
  }

  /* Many tabs (a torrent's download window) go on in a second row rather
     than off the edge. */
  .strip.wrap {
    flex-wrap: wrap;
    height: auto;
    row-gap: 2px;
  }

  .strip.wrap .tab {
    height: 24px;
    white-space: nowrap;
  }

  /* Tabs as Windows 11 draws a pivot: plain text, the current one bold
     with the accent's short bar under it. */
  .tab {
    position: relative;
    padding: 0 10px;
    background: none;
    border: 0;
    border-radius: var(--radius);
    color: var(--fg-muted);
    transition: background-color 0.08s ease-out;
  }

  .tab:hover {
    background: var(--hover);
    color: var(--fg);
  }

  .tab.on {
    color: var(--fg);
    font-weight: 600;
  }

  .tab.on::after {
    content: '';
    position: absolute;
    left: 50%;
    bottom: 0;
    width: 16px;
    height: 3px;
    margin-left: -8px;
    border-radius: 2px;
    background: var(--accent);
  }

  .page {
    flex: 1;
    background: var(--field);
    border: 1px solid var(--frame);
    border-radius: var(--radius-large);
    padding: 8px 14px;
  }

</style>
