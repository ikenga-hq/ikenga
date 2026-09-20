// Legacy /install route → /packages with the install sheet pre-opened on
// the Local-path tab. The unified package surface (replaces /packages +
// /packages/browse + /install) owns install UX now; this redirect keeps
// older nav links and external deep-links pointing somewhere alive.
//
// Plan: plans/shell/2026-05-17-pkg-surface-unify.md — Phase 5.

import { createFileRoute, redirect } from '@tanstack/react-router';
import { z } from 'zod';

const searchSchema = z.object({
	install: z.string().optional(),
});

export const Route = createFileRoute('/install')({
	beforeLoad: ({ search, location }) => {
		throw redirect({
			to: '/ngwa/installed',
			search: {
				...search,
				install: search.install ?? 'local-path',
			},
			hash: location.hash,
		});
	},
	validateSearch: searchSchema,
});

