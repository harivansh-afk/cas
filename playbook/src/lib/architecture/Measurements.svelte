<script lang="ts">
  import data from './measurements.json';
  const metrics = [
    { id: 'read', label: '4 KiB read', unit: 'ms', lower: true },
    { id: 'write', label: '4 KiB write', unit: 'ms', lower: true },
    { id: 'flush', label: 'fdatasync', unit: 'ms', lower: true },
    { id: 'sequential', label: '1 MiB read', unit: 'MiB/s', lower: false }
  ] as const;
  let selected = $state<(typeof metrics)[number]>(metrics[0]);
  const rows = $derived(data.arms.map(arm => {
    const result = arm.cases.find(result => result.case === selected.id)!;
    return { name: arm.label, value: result.metric.median, min: result.metric.min, max: result.metric.max };
  }));
  const largest = $derived(Math.max(...rows.map(row => row.value), 0.001));
  const number = (value: number) => value.toLocaleString('en-US', { maximumFractionDigits: 2 });
  const mib = (value: number) => Math.round(value / 1048576).toLocaleString('en-US');
</script>

<div class="measurements">
  <div class="choices" aria-label="Measurement">
    {#each metrics as metric}
      <button type="button" aria-pressed={selected.id === metric.id} onclick={() => selected = metric}>{metric.label}</button>
    {/each}
  </div>
  <p class="caption">{selected.id === 'sequential' ? 'Median throughput' : 'Median of each run’s p99'} · {selected.lower ? 'lower' : 'higher'} is better</p>
  <div aria-live="polite">
    {#each rows as row}
      <div class="row">
        <span>{row.name}</span>
        <strong>{number(row.value)} <small>{selected.unit}</small></strong>
        <div class="track"><div class="bar" style:width={`${row.value / largest * 100}%`}></div></div>
        <span class="range">Run range {number(row.min)}–{number(row.max)} {selected.unit}</span>
      </div>
    {/each}
  </div>
  <p class="caption">{data.repeats} × {data.seconds}s per case · 32 MiB file · direct IO, QD1 · caches retained between jobs</p>
  <details>
    <summary>Memory observed across each lab session</summary>
    <div class="memory-scroll">
      <table>
        <thead><tr><th>Backend</th><th>Lab peak</th><th>Storage PSS</th></tr></thead>
        <tbody>{#each data.arms as arm}<tr><td>{arm.label}</td><td>{mib(arm.lab_peak_bytes)} MiB</td><td>{arm.backend === 'raw' ? 'In QEMU' : `${mib(arm.sampled_storage_pss_peak_bytes)} MiB`}</td></tr>{/each}</tbody>
      </table>
    </div>
    <p class="caption">Lab peak: cgroup memory, including the outer VM. Storage PSS: sampled inner daemon peak. The columns overlap; do not add them. No cgroup OOM kills occurred.</p>
  </details>
</div>

<style>
  .measurements { margin: 1.5rem 0; border: 1px solid var(--border); padding: 1.25rem; border-radius: 0.5rem; }
  .choices { display: flex; flex-wrap: wrap; gap: 0.4rem; }
  button { font: inherit; font-size: 0.85rem; background: transparent; color: inherit; border: 1px solid #8886; padding: 0.45rem 0.7rem; border-radius: 0.25rem; cursor: pointer; }
  button[aria-pressed='true'] { background: var(--text-primary); color: var(--background); }
  button:focus-visible { outline: 2px solid currentColor; outline-offset: 3px; }
  .caption, .range { font-size: 0.78rem; opacity: 0.7; }
  .row { display: grid; grid-template-columns: 1fr auto; gap: 0.3rem 1rem; margin: 1.3rem 0; }
  .row > span:first-child { font-size: 0.9rem; }
  .track { grid-column: 1 / -1; background: #8882; height: 0.5rem; border-radius: 0.25rem; overflow: hidden; }
  .bar { height: 100%; background: currentColor; opacity: 0.7; }
  .range { grid-column: 1 / -1; }
  strong { font-variant-numeric: tabular-nums; }
  small { font-weight: normal; }
  details { border-top: 1px solid var(--border); padding-top: 1rem; }
  summary { font-size: 0.85rem; cursor: pointer; }
  .memory-scroll { overflow-x: auto; }
  table { width: 100%; border-collapse: collapse; font-size: 0.8rem; margin-top: 0.7rem; }
  th, td { text-align: right; padding: 0.6rem 0.4rem; border-bottom: 1px solid var(--border); white-space: nowrap; }
  th:first-child, td:first-child { text-align: left; }
</style>
