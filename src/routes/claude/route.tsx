// /claude — Legacy Claude/Ngwa route (WP-10).
//
// Redirects /claude to /ngwa/installed, preserving mapped search params
// (surface, scope, kind, sys) and hash.

import { createFileRoute, redirect } from '@tanstack/react-router';
import { z } from 'zod';

const ngwaSearchSchema = z.object({
	surface: z
		.enum(['browse', 'registry', 'store', 'graph', 'map', 'life', 'health', 'flow'])
		.optional()
		.catch(undefined),
	scope: z.string().optional(),
	kind: z.enum(['skills', 'agents', 'commands', 'hooks', 'mcps']).optional().catch(undefined),
	sys: z.string().optional(),
});

export const Route = createFileRoute('/claude')({
	beforeLoad: ({ search, location }) => {
		throw redirect({
			to: '/ngwa/installed',
			search: {
				surface: search.surface,
				scope: search.scope,
				kind: search.kind,
				sys: search.sys,
			},
			hash: location.hash,
		});
	},
	validateSearch: ngwaSearchSchema,
});
