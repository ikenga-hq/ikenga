import { describe, expect, it } from 'vitest';

import type { Project } from '@/lib/tauri-cmd';
import { projectIdForRoot } from './ngwa-helpers';

// Regression for the WP-08 scope-grammar parity gap: the FE used to emit
// `project:<basename>`, but the Rust store resolves `project:<id>` via
// `get_project(id)` where `id` is a slug set independently from `root_path`.
// `projectIdForRoot` must return the real slug so project-scoped enable/copy/
// move don't error `no project with id "<basename>"`.

function project(id: string, root_path: string | null): Project {
	return {
		id,
		display_name: id,
		root_path,
		icon: null,
		color: null,
		description: null,
		position: 0,
		is_default: false,
		created_at: 0,
		archived_at: null,
	};
}

describe('projectIdForRoot', () => {
	it('returns the slug id when it differs from the directory basename', () => {
		// The case that was silently broken: slug `ikenga-shell`, dir basename `shell`.
		const projects = [project('ikenga-shell', '/home/u/royalti-co/ikenga/shell')];
		expect(projectIdForRoot(projects, '/home/u/royalti-co/ikenga/shell')).toBe('ikenga-shell');
	});

	it('matches on exact root_path before basename', () => {
		const projects = [
			project('alpha', '/home/u/a/shell'),
			project('beta', '/home/u/b/shell'), // same basename, different path
		];
		expect(projectIdForRoot(projects, '/home/u/b/shell')).toBe('beta');
	});

	it('falls back to basename match when paths diverge (~ vs absolute)', () => {
		const projects = [project('my-proj', '~/code/my-proj')];
		// Scanned root is absolute; only the basename agrees.
		expect(projectIdForRoot(projects, '/home/u/code/my-proj')).toBe('my-proj');
	});

	it('tolerates trailing slashes on either side', () => {
		const projects = [project('site', '/srv/site/')];
		expect(projectIdForRoot(projects, '/srv/site')).toBe('site');
	});

	it('returns null when no project row matches (caller falls back to basename)', () => {
		const projects = [project('known', '/home/u/known')];
		expect(projectIdForRoot(projects, '/home/u/unknown')).toBeNull();
	});

	it('returns null for a null/empty root and ignores rows without a root_path', () => {
		expect(projectIdForRoot([project('x', null)], null)).toBeNull();
		expect(projectIdForRoot([project('x', null)], '/home/u/x')).toBeNull();
	});
});
