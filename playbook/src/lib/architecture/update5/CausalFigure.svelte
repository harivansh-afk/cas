<script lang="ts">
 import type { Panel } from './figures';
 let { panels, caption }: { panels: Panel[]; caption: string } = $props();
 const uid = $props.id();
</script>

<figure>
 <div class="panels">
  {#each panels as panel, i}
   <div class="panel">
    <p class="panel-title">{panel.title}</p>
    <svg viewBox="0 0 360 {panel.h}" role="img" aria-labelledby="title-{uid}-{i} desc-{uid}-{i}">
     <title id="title-{uid}-{i}">{panel.title}</title>
     <desc id="desc-{uid}-{i}">{panel.description}</desc>
     <defs>
      <marker id="arrow-{uid}-{i}" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="6" markerHeight="6" orient="auto-start-reverse"><path d="M1 1 L7 4 L1 7" fill="none" stroke="context-stroke" stroke-width="1.2" /></marker>
     </defs>
     {#each panel.edges as edge}
      <path d={edge.d} class:focus={edge.focus} class="edge" stroke-dasharray={edge.dashed ? '4 4' : undefined} marker-end="url(#arrow-{uid}-{i})" />
     {/each}
     {#each panel.boxes as box}
      <g class:focus={box.tone === 'focus'} class:muted={box.tone === 'muted'}>
       <rect x={box.x} y={box.y} width={box.w} height={box.h ?? 52} stroke-dasharray={box.dashed ? '4 4' : undefined} />
       {#if box.label}<text x={box.x + box.w / 2} y={box.y + (box.sub ? 22 : 30)}>{box.label}</text>{/if}
       {#if box.sub}<text class="sub" x={box.x + box.w / 2} y={box.y + 40}>{box.sub}</text>{/if}
      </g>
     {/each}
     {#each panel.labels ?? [] as label}<text class="annotation" class:focus={label.focus} x={label.x} y={label.y}>{label.text}</text>{/each}
    </svg>
   </div>
  {/each}
 </div>
 <figcaption>{caption}</figcaption>
</figure>

<style>
 figure { --wide: min(920px, 100vw - 3rem); margin-top: 1.6rem; margin-bottom: 1.6rem; gap: 0; }
 .panels { display: grid; grid-template-columns: 1fr 1fr; border: 1px solid var(--border); border-radius: 6px; overflow: hidden; }
 .panel + .panel { border-left: 1px solid var(--border); }
 .panel-title { padding: 1rem 1.25rem 0; margin: 0; font-size: 0.75rem; font-weight: 500; color: var(--text-primary); }
 svg { display: block; width: 100%; height: auto; color: var(--text-secondary); padding: 0.4rem; }
 rect { fill: none; stroke: currentColor; stroke-width: 1; }
 text { fill: currentColor; text-anchor: middle; font-family: var(--font-mono); font-size: 12px; }
 .sub { font-size: 10px; }
 .annotation { font-size: 10.5px; }
 .edge { fill: none; stroke: currentColor; stroke-width: 1.2; }
 .focus { color: #996616; }
 .focus.edge { stroke: #996616; }
 .muted { color: var(--text-tertiary); }
 figcaption { border: 0; padding: 0.75rem 0 0; margin: 0; text-align: left; font-size: 0.75rem; max-width: none; }
 @media (prefers-color-scheme: dark) { .focus { color: #d8b86b; } .focus.edge { stroke: #d8b86b; } }
 @media (max-width: 640px) {
  figure { --wide: 100%; }
  .panels { grid-template-columns: 1fr; }
  .panel + .panel { border-left: 0; border-top: 1px solid var(--border); }
  .panel-title { padding: 1rem 1rem 0; }
 }
 @media print { figure { --wide: 100%; break-inside: avoid; } }
</style>
