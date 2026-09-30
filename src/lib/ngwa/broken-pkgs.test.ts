import { describe, expect, it } from 'vitest';
import type { PkgHealthIssue } from '@/lib/tauri-cmd';
import { brokenPkgMap, markBrokenEntries, registryNameMatches } from './broken-pkgs';
import type { NgwaStoreEntry } from './enrichment';

const issue = (id: string, kind: PkgHealthIssue['issue']): PkgHealthIssue => ({
	id,
	install_path: `/pkgs/${id}`,
	enabled: false,
	issue: kind,
	detail: `${id}: ${kind.kind}`,
});

const entry = (name: string): NgwaStoreEntry =>
	({ id: name, name, installedItem: null, isUpdate: false }) as unknown as NgwaStoreEntry;

describe('broken pkgs (on disk, failed to register)', () => {
	it('keeps only the unregistered-pkg kinds', () => {
		const map = brokenPkgMap([
			issue('com.ikenga.meetings', { kind: 'pkgs_dir_unloadable' }),
			issue('com.ikenga.git', { kind: 'register_failed' }),
			issue('com.ikenga.gone', { kind: 'manifest_missing' }),
			issue('com.ikenga.orphan', { kind: 'orphan_row', table: 'pkg_settings' }),
		]);
		expect([...map.keys()]).toEqual(['com.ikenga.meetings', 'com.ikenga.git']);
	});

	it('matches registry npm names to manifest ids', () => {
		expect(registryNameMatches('@ikenga/pkg-meetings', 'com.ikenga.meetings')).toBe(true);
		expect(registryNameMatches('com.x.y', 'com.x.y')).toBe(true);
		expect(registryNameMatches('@ikenga/pkg-studio', 'com.ikenga.meetings')).toBe(false);
	});

	it('stamps the broken detail on the matching Store entry only', () => {
		const broken = brokenPkgMap([issue('com.ikenga.meetings', { kind: 'pkgs_dir_unloadable' })]);
		const out = markBrokenEntries([entry('@ikenga/pkg-meetings'), entry('@ikenga/pkg-studio')], broken);
		expect(out[0]!.broken).toBe('com.ikenga.meetings: pkgs_dir_unloadable');
		expect(out[1]!.broken).toBeUndefined();
	});
});
