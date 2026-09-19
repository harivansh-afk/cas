<script lang="ts">
	let { size = 16 }: { size?: number } = $props();
</script>

<!-- A page with a folded corner and three text lines. Same optical weight as the GitHub mark. -->
<svg viewBox="0 0 16 16" width={size} height={size} fill="none" stroke="currentColor" stroke-width="1.25" stroke-linejoin="round" stroke-linecap="round" aria-hidden="true">
	<path d="M4 1.5h5.5L13 5v9.5H4z" />
	<path d="M9.5 1.5V5H13" />
	<path d="M6 8h5M6 10.25h5M6 12.5h3" />
</svg>
