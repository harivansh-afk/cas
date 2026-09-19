<script lang="ts">
  import data from './pressure.json';
  import repeat from './congestion.json';
  import merged from './pressure-repeat.json';

  const trace = data.instrumented;
  const source = (path: string) => `https://git.harivan.sh/harivansh-afk/cas-research/src/commit/${trace.source_revision}/${path}`;
  const report = 'https://git.harivan.sh/harivansh-afk/cas-research/src/commit/97de945964ceefa671547d0a844b39b4769e0e66/docs/measurements/pressure-2026-09-14/README.md';
  const fix = 'https://git.harivan.sh/harivansh-afk/cas-research/src/commit/6db21e41f9ce2338a39b7e095552ff50f4990ea5/docs/congestion-wait.md';
  const repeatReport = 'https://git.harivan.sh/harivansh-afk/cas-research/src/commit/d35f19c37e76fd8c101c4f1984da8d0a2721b2e3/docs/measurements/congestion-wait-2026-09-14/README.md';
  const mergedReport = 'https://git.harivan.sh/harivansh-afk/cas-research/src/commit/fc2a28beb2659dcdd2726928e846355cb6bad32d/docs/measurements/pressure-repeat-2026-09-14/README.md';
  const timeline = trace.timeline;
  let guest = $state(0);
  let index = $state(timeline.findIndex(point => point.guests[0].errors > 0));
  const sample = $derived(timeline[index]);
  const guestSample = $derived(sample.guests[guest]);
  const x = (second: number) => 48 + second / timeline[timeline.length - 1].second * 660;
  const y = (mib: number) => 238 - mib / 280 * 206;
  const line = $derived(timeline.map(point => `${x(point.second)},${y(point.guests[guest].allocated_mib)}`).join(' '));
  const phases = [
    ['load', 'Read the WAL'], ['prepare', 'Hash + prepare tree'],
    ['chunks', 'Write / sync chunks'], ['manifest', 'Publish manifest'],
    ['reclaim', 'Publish D + reclaim WAL']
  ] as const;
  const total = Object.values(trace.phases_ns).reduce((sum, value) => sum + value, 0);
  const number = (value: number) => value.toFixed(1);
</script>

<div class="pressure">
  <p class="finding"><strong>Writes resumed when WAL space returned.</strong> All {merged.fio_jobs_passed} fio jobs completed without IO errors. Both logs drained, and both 64 MiB seeds verified after load and a full restart. <a href={mergedReport}>Measured on 14 September at {merged.source_revision.slice(0, 7)}</a>.</p>
  <p class="caption"><strong>Mixed-read latency still reached 10.5 seconds.</strong> No GC occurred. Both guests sent every request through queue 0, where writes waited for WAL space. Later reads can wait behind them; other-queue scheduling had no work to serve here. The evidence points to queue blocking, but does not time every phase of each read.</p>
  <div class="comparison" aria-label="Work per MiB compacted before and after the compaction changes">
    {#each merged.comparison as row}
      <div class="work-row">
        <div class="work-label"><strong>{row.label}</strong><span>{row.unit}</span></div>
        <div class="work-bars">
          <div class="work-bar"><span>Before</span><div class="track"><div class="earlier" style:width={`${row.before / Math.max(row.before, row.after) * 100}%`}></div></div><strong>{number(row.before)}</strong></div>
          <div class="work-bar"><span>After</span><div class="track"><div style:width={`${row.after / Math.max(row.before, row.after) * 100}%`}></div></div><strong>{number(row.after)}</strong></div>
        </div>
      </div>
    {/each}
  </div>
  <p class="caption">Work per MiB of compacted input: 878 MiB before, 1,173 MiB after. Counts include rotation and reclamation; neither run had GC. One repetition with different backlog and timing. These are operation counts, not physical-device traffic or a statistical speedup.</p>
  <details>
    <summary>Source revisions, drain observations and remaining costs</summary>
    <p class="caption">Before: <code>{repeat.source_revision.slice(0, 7)}</code>. After: <code>{merged.source_revision.slice(0, 7)}</code>, including the compaction changes in PR #49. The full report retains commands, counters and comparison limits.</p>
    <p class="caption">The sampled drain rate rose from 13.1 to 21.6 MiB/s. Initialization fell to 0.34 s, while reads took 40.20 s and syncs 31.71 s across the new progression. Reclamation still revisits retained WAL history. Measure its reads and syncs by caller, then reduce that repeated work. Within-queue read bypass needs an explicit ordering contract first.</p>
  </details>
  <details class="history">
    <summary>Earlier runs: how congestion became an IO error</summary>
    <p class="caption">Removing queue expiry let an <a href={repeatReport}>earlier repeat</a> complete through {number(repeat.max_finished_admission_wait_s[0])} / {number(repeat.max_finished_admission_wait_s[1])}-second waits with zero errors, but mixed reads reached 11.3 seconds and one GC paused the host for 3.17 seconds. The trace below predates that fix.</p>
  <p class="caption"><strong>Earlier failure, shown below.</strong> The 256 MiB image quota stayed closed for about 8.6–13.6 seconds while the WAL drained; queued heads expired after five seconds. <a href={fix}>The fix</a> keeps capacity waits pending and wakes admission when reclamation restores space. A separate native test also held an accepted write before submission for 31 seconds and completed it after release.</p>
  <div class="choices" aria-label="Guest staging trace">
    {#each [0, 1] as choice}<button type="button" aria-pressed={guest === choice} onclick={() => guest = choice}>Guest {choice + 1}</button>{/each}
  </div>
  <svg viewBox="0 0 740 280" role="img" aria-labelledby="pressure-title pressure-description">
    <title id="pressure-title">Per-image staging allocation during overload</title>
    <desc id="pressure-description">Allocation approaches 256 MiB. The image stops admitting writes, then drains toward the 60 percent reopen threshold. Orange shading marks sampled stopped admission. The host-wide quota remains open throughout.</desc>
    {#each timeline.slice(0, -1) as point, i}
      {#if point.guests[guest].stopped}<rect x={x(point.second)} y="26" width={x(timeline[i + 1].second) - x(point.second) + 0.2} height="212" class="stopped" />{/if}
    {/each}
    <line x1="48" x2="708" y1={y(256)} y2={y(256)} class="threshold" />
    <line x1="48" x2="708" y1={y(153.6)} y2={y(153.6)} class="threshold" />
    <text x="48" y={y(256) - 8}>256 MiB cap</text>
    <text x="48" y={y(153.6) - 8}>153.6 MiB reopen threshold</text>
    <polyline points={line} fill="none" stroke="currentColor" stroke-width="2.5" />
    <line x1={x(sample.second)} x2={x(sample.second)} y1="26" y2="238" class="cursor" />
    <circle cx={x(sample.second)} cy={y(guestSample.allocated_mib)} r="4" fill="currentColor" />
    <text x="48" y="263">0 s</text><text x="708" y="263" text-anchor="end">{number(timeline[timeline.length - 1].second)} s</text>
  </svg>
  <label for="pressure-time">Inspect the trace: {number(sample.second)} s</label>
  <input id="pressure-time" type="range" min="0" max={timeline.length - 1} step="1" bind:value={index} />
  <p class="reading" aria-live="polite"><strong>{number(guestSample.allocated_mib)} MiB</strong> · write quota {guestSample.stopped ? 'closed' : 'open'} · {guestSample.blocked_heads} blocked queue head{guestSample.blocked_heads === 1 ? '' : 's'} · {guestSample.errors} guest IO error{guestSample.errors === 1 ? '' : 's'}</p>
  <p class="caption">Orange = write quota closed. Reserving the next 8 MiB segment can stop writes before allocation reaches 256 MiB. Samples are about 500 ms apart; a threshold crossing and a new allocation can occur between them.</p>
  <details>
    <summary>Historical compactor phase totals</summary>
    {#each phases as [key, label]}
      <div class="phase"><span>{label}</span><strong>{number(trace.phases_ns[key] / total * 100)}%</strong><div class="track"><div style:width={`${trace.phases_ns[key] / total * 100}%`}></div></div></div>
    {/each}
    <p class="caption">Elapsed phase time across this lab, including seeding and drain. Disk reservation was under 0.1%. Preparation combines hashing and tree edits; these are not CPU samples or a hash-only profile.</p>
  </details>
  <details>
    <summary>The wake-up gap now has a regression test</summary>
    <p class="caption">The original trace showed Guest 2’s quota reopen while its head stayed blocked. A deterministic test then reproduced the missing notification without unrelated IO. It passes after the fix; this does not establish the exact thread interleaving of the historical guest run.</p>
  </details>
  <details>
    <summary>What disk and memory controls established</summary>
    <p class="caption">With a restricted physical-space budget, GC could not reopen writes, but all {data.capacity.control.read_bytes / 1048576} MiB of seeded data remained readable. Native memory-budget tests refused index growth and compaction preparation before output; a read-page allocation denial failed its image. Actual kernel OOM and unexpected filesystem ENOSPC remain untested.</p>
  </details>
  <p class="caption">Unmodified build: EIO at 1 MiB / QD32 after the earlier stages. Instrumented repeat: EIO already at 4 KiB / QD64. The stage varies with backlog; neither is a universal queue-depth limit. No physical-space pressure, GC or cgroup OOM explained these failures. Seed checks passed, and writes resumed after drain.</p>
  <p class="caption"><a href={report}>Experiment, controls and evidence</a> · measured at <code>{trace.source_revision.slice(0, 7)}</code>. New instrumentation in <code>cas-daemon</code>: <a href={source('crates/cas/daemon/src/local/pressure.rs')}>local/pressure.rs</a> and <a href={source('crates/cas/daemon/src/local/host/statistics.rs')}>local/host/statistics.rs</a>.</p>
  </details>
</div>

<style>
  .pressure { border: 1px solid var(--border); padding: 1.25rem; border-radius: 0.5rem; margin: 1.5rem 0; }
  .finding { margin-top: 0; }
  .choices { display: flex; gap: 0.5rem; }
  button { font: inherit; font-size: 0.85rem; background: transparent; color: inherit; border: 1px solid #8886; border-radius: 0.25rem; padding: 0.45rem 0.7rem; cursor: pointer; }
  button[aria-pressed='true'] { background: var(--text-primary); color: var(--background); }
  button:focus-visible, input:focus-visible { outline: 2px solid currentColor; outline-offset: 3px; }
  svg { width: 100%; height: auto; margin-top: 1rem; overflow: visible; }
  svg text { fill: currentColor; font: 13px ui-monospace, monospace; }
  .threshold { stroke: currentColor; opacity: 0.3; stroke-dasharray: 4 4; }
  .cursor { stroke: currentColor; opacity: 0.45; }
  .stopped { fill: #d9942530; }
  label, .reading { font-size: 0.85rem; }
  input { display: block; width: 100%; margin: 0.7rem 0; accent-color: #ad711b; }
  .caption { font-size: 0.78rem; opacity: 0.75; }
  .caption a { overflow-wrap: anywhere; }
  .phase { display: grid; grid-template-columns: 1fr auto; gap: 0.25rem 1rem; margin: 0.8rem 0; font-size: 0.85rem; }
  .track { grid-column: 1 / -1; background: #8882; height: 0.4rem; }
  .track > div { background: currentColor; height: 100%; opacity: 0.7; }
  .comparison { margin: 1.4rem 0; }
  .work-row { display: grid; grid-template-columns: minmax(8rem, 1fr) 2fr; gap: 1rem; padding: 0.8rem 0; border-bottom: 1px solid var(--border); font-size: 0.8rem; }
  .work-label { display: flex; flex-direction: column; gap: 0.2rem; }
  .work-label span { opacity: 0.65; font-size: 0.7rem; }
  .work-bars { display: grid; gap: 0.5rem; }
  .work-bar { display: grid; grid-template-columns: 5rem minmax(2rem, 1fr) 3rem; gap: 0.5rem; align-items: center; }
  .work-bar > span { font-size: 0.7rem; opacity: 0.75; }
  .work-bar > strong { text-align: right; font-variant-numeric: tabular-nums; }
  .work-bar .track { grid-column: auto; height: 0.5rem; }
  .track > .earlier { opacity: 0.3; }
  .history { padding-top: 0.8rem; border-top: 1px solid var(--border); }
  details { margin-top: 1rem; }
  @media (max-width: 540px) { svg text { font-size: 24px; } .work-row { grid-template-columns: 1fr; gap: 0.6rem; } }
</style>
