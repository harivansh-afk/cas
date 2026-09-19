import { error } from '@sveltejs/kit';
import { updates } from '$lib/updates';
import type { EntryGenerator, PageLoad } from './$types';

// Each deck is a directory index, so the URL works with or without a trailing slash.
export const trailingSlash = 'always';

export const entries: EntryGenerator = () => updates.map(({ slug }) => ({ slug }));

export const load: PageLoad = ({ params }) => {
	const update = updates.find(({ slug }) => slug === params.slug);
	if (!update) error(404, 'Update not found');
	return { update };
};
