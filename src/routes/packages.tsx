// Unified package surface. Outer chrome is the title strip + (conditional)
// trust banner + grouped catalog rows; detail is the right-sliding loupe
// (Overview · Permissions · Trust · Settings · Manifest). Filters/counts
// live in the Packages-mode sidebar via ?filter=. Install lives in a sheet
// triggered by the title-strip button or sidebar's "Install from path"
// item (which deep-links via ?install=local-path).
//
// Design source: design/shell/concepts/04-pkgs/04-package-manager/v2-mission-control.html
// Plan:         plans/shell/2026-05-17-pkg-surface-unify.md

import { createFileRoute, redirect } from '@tanstack/react-router';
import { z } from 'zod';

const FILTER_VALUES = ['all', 'installed', 'updates', 'store', 'review', 'disabled'] as const;
const INSTALL_VALUES = ['manifest-url', 'local-path', 'registry'] as const;

const searchSchema = z.object({
	filter: z.enum(FILTER_VALUES).optional(),
	install: z.enum(INSTALL_VALUES).optional(),
});

export const Route = createFileRoute('/packages')({
	beforeLoad: ({ search, location }) => {
		const to = search.filter === 'store' ? '/ngwa/store' : '/ngwa/installed';
		throw redirect({
			to,
			search,
			hash: location.hash,
		});
	},
	validateSearch: searchSchema,
});

