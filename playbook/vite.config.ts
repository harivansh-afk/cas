import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [
		sveltekit({
			compilerOptions: {
				// Force runes mode for the project, except for libraries. Can be removed in svelte 6.
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true
			},

			// Fully prerendered: every route is static HTML.
			adapter: adapter(),

			// Set by the GitHub Pages workflow (e.g. /playbook); empty for local dev.
			paths: { base: (process.env.BASE_PATH || '') as '' | `/${string}` },

			// spec.pdf is typeset from the built pages (scripts/pdf), so it does
			// not exist yet when the crawler follows the index link to it.
			prerender: {
				handleHttpError: ({ path, message }) => {
					if (path.endsWith('/spec.pdf')) return;
					throw new Error(message);
				},
				// Update 04's #node-<id> fragments select a box in its diagram; a
				// collapsed box has no element until the reader expands it.
				handleMissingId: ({ id, message }) => {
					if (id.startsWith('node-')) return;
					throw new Error(message);
				}
			}
		})
	],
	// The ELK layout engine behind Update 04 is one large chunk, loaded only
	// when a reader changes the diagram's view.
	build: { chunkSizeWarningLimit: 2000 }
});
