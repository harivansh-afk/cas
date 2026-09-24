<script lang="ts">
 import { base } from '$app/paths';
 import '$lib/architecture/article.css';
 import CausalFigure from '$lib/architecture/update5/CausalFigure.svelte';
 import { architecture, reads, capacity, compaction, bypass, scheduler, testbed } from '$lib/architecture/update5/figures';
 import Measurements from '$lib/architecture/Measurements.svelte';
 import Pressure from '$lib/architecture/Pressure.svelte';
 import ReadPressure from '$lib/architecture/ReadPressure.svelte';
 import ReadDecision from '$lib/architecture/ReadDecision.svelte';
 import ReadProgress from '$lib/architecture/ReadProgress.svelte';
 import Integration from '$lib/architecture/Integration.svelte';
 import work from '$lib/architecture/pressure-repeat.json';
 import data from '$lib/architecture/native.json';
 import type { NativeArm, NativeMetric, NativeReport, NativeValue } from '$lib/architecture/native';
 const report = data as NativeReport;
 const revision = 'ecc39e49fcb66ab28a8d16018de04a589273c304';
 const source = (path: string) => `https://git.harivan.sh/harivansh-afk/cas/src/commit/${revision}/${path}`;
 const nativeSource = (path: string) => `https://git.harivan.sh/harivansh-afk/cas/src/commit/${report.source_revision}/${path}`;
 const number = (v: number) => v.toLocaleString('en-US', { maximumFractionDigits: 2 });
 const arms: { id: NativeArm; label: string }[] = [{ id: 'raw', label: 'Raw XFS' }, { id: 'daemon', label: 'Passthrough' }, { id: 'cas', label: 'CAS' }];
 const result = (value?: NativeValue) => value ? number(value.median) : 'No result';
 const range = (value?: NativeValue) => value ? `${number(value.min)} to ${number(value.max)}. ${value.n} runs.` : '';
</script>

<svelte:head>
 <title>Update 05 · How CAS changed</title>
 <meta name="description" content="CAS write and read paths, capacity waits, compaction work, read bypass, scheduler rotation, and the move from nested Spark tests to direct KVM on CloudLab." />
</svelte:head>

{#snippet table(rows: NativeMetric[])}
 <!-- svelte-ignore a11y_no_noninteractive_tabindex (Scrollable tables need keyboard focus for horizontal scrolling.) -->
 <div class="table-scroll" role="region" aria-label="Native measurements" tabindex="0"><table class="spec">
  <thead><tr><th scope="col">Measurement</th>{#each arms as arm}<th scope="col">{arm.label}</th>{/each}</tr></thead>
  <tbody>{#each rows as row}<tr><th scope="row">{row.label}<small>{row.unit}</small></th>{#each arms as arm}<td>{result(row.values[arm.id])}<small>{range(row.values[arm.id])}</small></td>{/each}</tr>{/each}</tbody>
 </table></div>
{/snippet}

<article class="architecture update-five" id="beginning">
 <header>
  <a class="back" href="{base}/">Index</a>
  <h1>How CAS changed</h1>
  <p class="date">Update 05 · 24 September 2026</p>
  <p>CAS shares identical disk blocks across VM images. Writes enter a per-image log. Background compaction moves durable data into shared chunks.</p>
  <p>Under write pressure, reads stalled for seconds. The changes below separate the capacity problem, the compaction cost, and the scheduler bug.</p>
 </header>

 <nav aria-label="On this page">
  <a href="#architecture">Write and read paths</a>
  <a href="#capacity">Capacity waits</a>
  <a href="#compaction">Compaction work</a>
  <a href="#bypass">Read discovery</a>
  <a href="#scheduler">Scheduler rotation</a>
  <a href="#cloudlab">CloudLab</a>
 </nav>

 <section id="architecture">
  <h2>Writes enter the log before shared storage</h2>
  <p>Each guest has its own disk image. QEMU exposes the guest's requests to CAS through shared-memory queues. Admission reserves capacity before CAS accepts a write.</p>
  <CausalFigure panels={architecture} caption="The write path and the background path. FLUSH does not wait for compaction." />
  <p>The write-ahead log, or WAL, holds recent writes. A WRITE completion means the write is published. An ordered FLUSH makes the log durable on the local host.</p>
  <p>The compactor hashes durable data in 4 KiB chunks. It stores each missing chunk once. A manifest maps each image's disk blocks to chunk hashes. CAS commits that map before reclaiming the covered log data.</p>
  <h3>Reads use the newest mapping</h3>
  <CausalFigure panels={reads} caption="The WAL overlay takes precedence. For older data, the manifest identifies the shared chunk." />
  <p>A read can combine recent WAL data with shared chunks. Holes return zeros. CAS keeps source pins until the read completes, so reclamation cannot remove data still in use.</p>
  <details><summary>Durability and source</summary>
   <p>Published writes, durable WAL, and compacted mappings are separate boundaries. The manifest cannot cover writes beyond the durable WAL. Reclamation also waits for reader and replay pins. Host garbage collection is separate from ordinary background compaction.</p>
   <p><a href={source('docs/storage-design.md')}>Storage design</a> · <a href={source('crates/cas/daemon/src/local/reactor/read.rs')}>Read routing</a> · <a href={source('crates/cas/core/src/append/compaction/output.rs')}>Manifest publication</a></p>
  </details>
 </section>

 <section id="capacity">
  <h2>A full log should delay a write</h2>
  <p>The first failure was a capacity timeout. Writes filled their WAL quota faster than compaction could reclaim it. After five seconds, an admission wait became a guest IO error.</p>
  <CausalFigure panels={capacity} caption="The change removes the capacity deadline. It does not make space available sooner." />
  <p>CAS now keeps the request pending. Its unadmitted payload stays in guest RAM. Reclamation wakes admission when space returns. A periodic retry covers releases without a wake notification.</p>
  <p class="result">The reproduced timeout errors disappeared. Writes resumed after waits of 12.66 and 13.60 seconds. Reads still took up to 11.34 seconds.</p>
  <details><summary>Measurement scope and source</summary>
   <p>14 September, two bounded Spark repeats. The two mixed-read maxima were 11.34 and 11.15 seconds. This fixes the reproduced capacity error, not all IO errors or maximum latency. Device and lifecycle deadlines still exist.</p>
   <p><a href={source('playbook/src/lib/architecture/congestion.json')}>Capacity-wait records</a> · <a href={source('crates/cas/daemon/src/backend/admission.rs')}>Admission and retry</a></p>
  </details>
 </section>

 <section id="compaction">
  <h2>The compactor repeated work it could skip</h2>
  <p>Selection rescanned old log prefixes. Buffer preparation initialized the maximum capacity. Manifest output included pages superseded by later edits in the same batch.</p>
  <CausalFigure panels={compaction} caption="A cursor skips processed framing. Smaller preparation and final-page emission reduce work per batch." />
  <p>Selection now resumes from a validated cursor. Buffers grow as needed. The final output keeps only reachable new manifest pages.</p>
  <!-- svelte-ignore a11y_no_noninteractive_tabindex (Scrollable tables need keyboard focus for horizontal scrolling.) -->
 <div class="table-scroll" role="region" aria-label="Compaction work measurements" tabindex="0"><table class="spec work-table">
   <caption>Work per MiB compacted · Spark, 14 September</caption>
   <thead><tr><th scope="col">Work</th><th scope="col">Before</th><th scope="col">After</th></tr></thead>
   <tbody>{#each work.comparison as row}<tr><th scope="row">{row.label}<small>{row.unit.replace(' / ', ' per ')}</small></th><td>{number(row.before)}</td><td>{number(row.after)}</td></tr>{/each}</tbody>
  </table></div>
  <p class="result">Repeated work fell. Sync calls did not. Mixed-read maxima were still 10.50 and 9.57 seconds.</p>
  <p class="note">These runs had different backlogs. The table counts work per MiB. It does not establish a device speedup or a read-latency improvement.</p>
  <details><summary>What still scans</summary>
   <p>The cursor is a validated hint with a safe scan fallback. Reclamation still walks retained history. Manifest edits can still create temporary paths in memory; final emission omits superseded pages.</p>
   <p><a href={source('playbook/src/lib/architecture/pressure-repeat.json')}>Compaction records</a> · <a href={source('crates/cas/core/src/append/compaction.rs')}>Selection cursor</a></p>
  </details>
 </section>

 <section id="bypass">
  <h2>A blocked write hid reads behind it</h2>
  <p>The frontend stopped at a write that could not get WAL capacity. A later read of unrelated blocks stayed undiscovered, even though it needed no write space.</p>
  <CausalFigure panels={bypass} caption="Discovery and admission are separate steps. Only independent reads can pass the blocked write." />
  <p>The frontend now retains a bounded set of request descriptors, then selects eligible reads. It does not copy an unlimited queue of write payloads.</p>
  <p>A read still waits behind an earlier overlapping write, zero, or discard. It also respects FLUSH barriers. This preserves the image's ordering rules.</p>
  <p class="result">Bypass occurred in the measured runs. Read p99 fell in the matched same-CPU comparison, but one read still took 6.834 seconds.</p>
  <p>The remaining trace exposed another problem. A read could be discovered and eligible, yet still lose every scheduler turn.</p>
  <details><summary>Measurement scope and source</summary>
   <p>14 September. The comparison used matched settings in separate sessions. It is distinct from the same-session control below. After admission, reads also retain the existing image-wide publication dependency.</p>
   <p><a href={source('playbook/src/lib/architecture/read-progress.json')}>Read-bypass records</a> · <a href={source('crates/cas/daemon/src/backend/frontier.rs')}>Discovery and ordering checks</a></p>
  </details>
 </section>

 <section id="scheduler">
  <h2>The scheduler kept retrying the refused write</h2>
  <p>With two images, each turn could retry the same capacity-blocked write. The scheduler moved to the other image, then repeated. Eligible reads in both images waited behind those retries.</p>
  <CausalFigure panels={scheduler} caption="A and B are images. These are scheduler tickets, not guest queue positions or write publication order." />
  <p>A refused, uncommitted ticket now moves to the back of its image's list. On the next visit, an eligible read can run. The scheduler still shares service between images using byte deficits.</p>
  <!-- svelte-ignore a11y_no_noninteractive_tabindex (Scrollable tables need keyboard focus for horizontal scrolling.) -->
 <div class="table-scroll" role="region" aria-label="Scheduler latency measurements" tabindex="0"><table class="spec latency-table">
   <caption>Same-session, same-CPU control · Spark, 15 September</caption>
   <thead><tr><th scope="col">Measurement</th><th scope="col">Before rotation</th><th scope="col">After rotation</th></tr></thead>
   <tbody>
    <tr><th scope="row">Mixed-read p99</th><td>219 to 287 ms</td><td>9 to 20 ms</td></tr>
    <tr><th scope="row">Independent bypass admission, max</th><td>8.110 s</td><td>≤ 0.137 s</td></tr>
   </tbody>
  </table></div>
  <p class="result">Read p99 improved. Writers continued to progress. One fixed run still had a 25.529-second read.</p>
  <p>The 137 ms bound above applies only to independent bypass admission. It is not a bound on all read latency. The remaining maximum stalls need tracing.</p>
  <details><summary>What this result establishes</summary>
   <p>The same-session control supports the scheduler change. It does not establish unchanged writer throughput or solve every read stall. The untraced ordinary-head maxima are consistent with FLUSH or WAL-capacity blocking, but their individual causes remain unproven.</p>
   <p><a href={source('playbook/src/lib/architecture/integration.json')}>Same-session records</a> · <a href={source('crates/cas/daemon/src/local/host/fair.rs')}>Refused-ticket rotation</a></p>
  </details>
 </section>

 <section id="cloudlab">
  <h2>CloudLab removes the nested guest setup</h2>
  <p>The Spark results above came from a development fixture. Workload guests used TCG emulation inside an outer KVM VM. Other work also ran on Spark.</p>
  <CausalFigure panels={testbed} caption="The completed CloudLab checks change the execution path. They do not yet measure dedicated-NVMe performance." />
  <p>The CloudLab host has an AMD EPYC 9354P, 32 cores, 192 GB RAM, and two 800 GB NVMe drives. CAS runs on the physical host. Guests run directly under KVM.</p>
  <p class="result">{report.status}</p>
  <p>Raw XFS is the storage baseline. Passthrough adds the daemon path without CAS storage. CAS adds the log, sharing, and compaction. The runner uses the same guest, workload, and resource limits for all three.</p>
  {#if report.baseline.length || report.pressure.length || report.accounting.length}
   <details><summary>Native measurement records</summary>
    {#if report.baseline.length}{@render table(report.baseline)}{/if}
    {#if report.pressure.length}<h3>Reader and writer in separate guests</h3>{@render table(report.pressure)}{/if}
    {#if report.accounting.length}<h3>Finite write and compaction drain</h3>{@render table(report.accounting)}{/if}
    <p>Cells show median, range, and run count. {report.completed} runs completed; {report.failed} attempts failed.</p>
    {#each report.failed_runs as run}<p>{run.name}. {run.backend}. {run.error}</p>{/each}
    {#each report.notes as note}<p>{note}</p>{/each}
   </details>
  {/if}
  <details><summary>Native runner settings</summary>
   <ul>
    <li>Each guest has two vCPUs, 2 GiB RAM, and one queue of 128 entries.</li>
    <li>Each run has an 8 GiB cgroup limit and no swap.</li>
    <li>The runner pins CPU affinity and memory policy to the storage NUMA node.</li>
    <li>fio uses direct IO and a 512 MiB working set. CRC checks verify the data before and after.</li>
    <li>CAS has a 16 MiB clean cache. Jobs retain cache state within a repeat. Each repeat starts with new storage.</li>
    <li>Read-only jobs wait for compaction to catch up. Write completion and fdatasync latency are separate measurements.</li>
    <li>The pressure case uses separate reader and writer guests. It does not test same-image FLUSH stalls.</li>
   </ul>
   <p><a href={nativeSource('docs/native-benchmark.md')}>Runner and method</a> · <a href={nativeSource('experiments/native/workload.sh')}>Workload</a> · <code>{report.source_revision.slice(0,7)}</code></p>
  </details>
 </section>

 <section id="next">
  <h2>The next result needs dedicated storage</h2>
  <ul>
   <li>Run raw XFS, passthrough, and CAS on the dedicated NVMe. Measure latency, physical storage, device writes, and total memory.</li>
   <li>Rerun recovery on that exact revision. Complete the allocation audit.</li>
   <li>Trace the remaining maximum read stalls.</li>
   <li>Add a ZFS comparison before claiming a storage advantage.</li>
  </ul>
  <p>Cross-host sharing, remote reads, replicated durability, and migration remain unimplemented.</p>
 </section>

 <section id="evidence">
  <h2>Measurement records</h2>
  <p>The explanations above stand on their own. These dated panels retain the detailed Spark measurements. Compare values within each experiment.</p>
  <details><summary>13 September · raw, passthrough, and CAS</summary><Measurements /></details>
  <details><summary>14 September · capacity and compaction</summary><Pressure /></details>
  <details><summary>14 September · read traces and bypass</summary><ReadPressure /><ReadDecision /><ReadProgress /></details>
  <details><summary>15 September · scheduler control</summary><Integration /></details>
 </section>
 <footer><a href="{base}/">Index</a><a href="#beginning">Top</a></footer>
</article>

<style>
 .date { color: var(--text-tertiary); font-size: 0.75rem; margin-top: -0.5rem; margin-bottom: 1.75rem; }
 section > h2::before { content: none; }
 .update-five > section { margin-top: 4rem; }
 .update-five > section > h2 { font-size: 1.125rem; margin-top: 0; margin-bottom: 1rem; }
 .update-five > nav { display: flex; flex-wrap: wrap; gap: 0.65rem 1.4rem; padding: 1rem 0; border: 0; border-top: 1px solid var(--border); border-bottom: 1px solid var(--border); border-radius: 0; font-size: 0.75rem; }
 .table-scroll:focus-visible { outline: 2px solid var(--text-primary); outline-offset: 4px; }
 .result { color: var(--text-primary); font-weight: 500; }
 ul { padding-left: 1.25rem; }
 li { margin-bottom: 0.5rem; }
 small { display: block; margin-top: 0.25rem; font-size: 0.6875rem; color: var(--text-tertiary); font-weight: normal; }
 caption { text-align: left; padding: 0.75rem; font-size: 0.6875rem; color: var(--text-tertiary); border-bottom: 1px solid var(--border); }
 .work-table, .latency-table { width: 100%; }
 .work-table td, .work-table th, .latency-table td, .latency-table th { min-width: 0; }
 .work-table td, .latency-table td { white-space: nowrap; font-variant-numeric: tabular-nums; }
 .spec thead th { text-transform: none; letter-spacing: 0; font-size: 0.75rem; }
 details > h3 { margin-top: 2rem; font-size: 0.875rem; }
 @media (max-width: 640px) { .update-five > section { margin-top: 3rem; } .update-five > section > h2 { font-size: 1rem; } .latency-table { min-width: 520px; } }
 @media print { .update-five > section { margin-top: 2rem; } details { display: none; } }
</style>
