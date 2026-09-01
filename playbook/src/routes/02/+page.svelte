<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="02" />
<ul class="reqs">
	<li><span class="rid">VMM-1</span>The backend plugs into the existing rust-vmm VMM behind one backend trait. Two implementations ship: raw-file and cas.</li>
	<li><span class="rid">VMM-2</span>The backend implements READ, WRITE, FLUSH, DISCARD, WRITE_ZEROES. GET_ID is trivial.</li>
	<li><span class="rid">VMM-3</span>Descriptor chains point into guest RAM. The VMM maps guest memory. The backend reads and writes guest buffers directly. No copy at the boundary.</li>
	<li><span class="rid">VMM-4</span>The device reports a volatile write cache. Data is durable only after an acked FLUSH. These are qemu <code>cache=writeback</code> semantics. The guest kernel already understands them.</li>
	<li><span class="rid">VMM-5</span>The backend drains a batch of requests per ring notification. In-flight requests run up to the queue depth.</li>
	<li><span class="rid">VMM-6</span>File IO goes through io_uring.</li>
	<li><span class="rid">VMM-7</span>All comparison arms run inside this VMM: raw-file vs cas, plus raw-file on a VDO device as the kernel-inline-dedup arm. qemu/qcow2 numbers are context only; a cross-VMM comparison mixes VMM variables into the measurement.</li>
</ul>

<PageNav num="02" />
