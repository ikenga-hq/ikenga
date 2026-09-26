// WP-60 — unit tests for the Keys tab's pure data derivation (written under
// DEC-50, not run here; WP-63 runs them). Builds real `EffectiveModel`
// fixtures through `buildEffectiveModel` (the same seam `merge.test.ts`
// uses) rather than hand-rolling one, so these tests exercise the actual
// merge this tab reads.

import { describe, expect, it } from 'vitest';
import { buildEffectiveModel, type MergeInput } from '@/lib/actions/merge';
import type { PackageActionSource } from '@/lib/actions/registry';
import type {
	ActionsDocument,
	ActionsFilesResult,
	ActionsFileState,
	ActionsScope,
	KeybindingRule,
	KeybindingsDocument,
	TrustState,
	UserAction,
} from '@/lib/actions/types';
import { buildKeyRows, conflictsForRow, matchesKey, matchesQuery, otherEntry, whenLabel } from './rows';

// ─── Fixtures (mirrors `src/lib/actions/merge.test.ts`) ────────────────────

function fileState<D extends ActionsDocument | KeybindingsDocument>(
	kind: 'actions' | 'keybindings',
	scope: ActionsScope,
	document: D | null
): ActionsFileState<D> {
	return {
		kind,
		scope,
		path: `/${scope}/.ikenga/${kind}.json`,
		present: document != null,
		document,
		stale: false,
		validation: { errors: [], warnings: [] },
		error: null,
	};
}

interface FilesOpts {
	personalActions?: ActionsDocument;
	personalBindings?: KeybindingRule[];
	projectActions?: ActionsDocument;
	projectBindings?: KeybindingRule[];
	projectTrust?: TrustState;
}

function makeFiles(opts: FilesOpts = {}): ActionsFilesResult {
	const kb = (bindings?: KeybindingRule[]): KeybindingsDocument | null => (bindings ? { version: 1, bindings } : null);
	return {
		personal: {
			scope: 'personal',
			actions: fileState('actions', 'personal', opts.personalActions ?? null),
			keybindings: fileState('keybindings', 'personal', kb(opts.personalBindings)),
		},
		project: {
			scope: 'project',
			actions: fileState('actions', 'project', opts.projectActions ?? null),
			keybindings: fileState('keybindings', 'project', kb(opts.projectBindings)),
		},
		projectId: 'p1',
		projectRoot: '/work/p1',
		projectKeybindingsTrust: opts.projectBindings
			? { hash: 'h', ruleCount: opts.projectBindings.length, state: opts.projectTrust ?? 'trusted' }
			: { hash: null, ruleCount: 0, state: 'absent' },
		trustError: null,
	};
}

function userAction(id: string, scope: ActionsScope, extra: Partial<UserAction> = {}): UserAction {
	return { id, name: `Action ${id}`, scope, run: { kind: 'open', url: `/${id}` }, ...extra };
}

function pkgAction(p: Partial<PackageActionSource> & Pick<PackageActionSource, 'pkgId' | 'localId'>): PackageActionSource {
	return {
		id: `${p.pkgId}:${p.localId}`,
		name: p.localId,
		origin: 'context_action',
		run: { kind: 'view', route: '/settings' },
		selector: { kind: 'file' },
		keyRequest: null,
		installedAt: 1,
		...p,
	};
}

function model(input: Partial<MergeInput> = {}) {
	return buildEffectiveModel({ files: null, packages: [], ...input });
}

// ─── buildKeyRows ────────────────────────────────────────────────────────────

describe('buildKeyRows', () => {
	it('with no files and no packages, every row is `bound` and comes from the defaults', () => {
		const m = model();
		const rows = buildKeyRows(m, 'mac');
		expect(rows.length).toBeGreaterThan(0);
		expect(rows.every((r) => r.kind === 'bound')).toBe(true);
		expect(rows.every((r) => r.source === 'default')).toBe(true);
	});

	it('a package key request held by a default binding gets its own `requested` row, never a `bound` one (§7.4)', () => {
		const m = model({
			// `mod+b` is `explorer.toggle`'s default — nothing frees it.
			packages: [pkgAction({ pkgId: 'com.x', localId: 'stage', keyRequest: 'mod+b' })],
		});
		const rows = buildKeyRows(m, 'mac');
		const requested = rows.find((r) => r.command === 'com.x:stage');
		expect(requested?.kind).toBe('requested');
		expect(requested?.key).toBeNull();
		expect(requested?.heldBy).toMatchObject({ kind: 'binding', command: 'explorer.toggle' });
		expect(rows.some((r) => r.kind === 'bound' && r.command === 'com.x:stage')).toBe(false);
	});

	it('a granted package request is a `bound` row, not a `requested` one', () => {
		const m = model({
			packages: [pkgAction({ pkgId: 'com.x', localId: 'stage', keyRequest: 'mod+shift+g' })],
		});
		const rows = buildKeyRows(m, 'mac');
		const bound = rows.find((r) => r.command === 'com.x:stage');
		expect(bound?.kind).toBe('bound');
		expect(bound?.key).toBe('mod+shift+g');
		expect(rows.some((r) => r.kind === 'requested' && r.command === 'com.x:stage')).toBe(false);
	});

	it('an untrusted project keybindings file produces `held` rows, not `bound` ones (DEC-65)', () => {
		const projectBindings: KeybindingRule[] = [
			{ key: 'enter', command: 'delete', when: 'filesFocus' },
			{ key: 'mod+w', command: '-pane.close' },
		];
		const m = model({ files: makeFiles({ projectBindings, projectTrust: 'untrusted' }) });
		const rows = buildKeyRows(m, 'mac');
		const held = rows.filter((r) => r.kind === 'held');
		expect(held).toHaveLength(2);
		expect(held.map((r) => r.command)).toEqual(['delete', 'pane.close']);
		expect(held.every((r) => r.held?.trust === 'untrusted')).toBe(true);
		// The held rules are inert — they never appear as `bound` rows too.
		expect(rows.some((r) => r.kind === 'bound' && r.source === 'project')).toBe(false);
	});

	it('a trusted project keybindings file produces ordinary `bound` rows, no `held` ones', () => {
		const projectBindings: KeybindingRule[] = [{ key: 'mod+shift+u', command: 'ours' }];
		const m = model({
			files: makeFiles({
				projectActions: { version: 1, actions: [userAction('ours', 'project')] },
				projectBindings,
				projectTrust: 'trusted',
			}),
		});
		const rows = buildKeyRows(m, 'mac');
		expect(rows.some((r) => r.kind === 'held')).toBe(false);
		const bound = rows.find((r) => r.command === 'ours');
		expect(bound).toMatchObject({ kind: 'bound', source: 'project', key: 'mod+shift+u' });
	});
});

// ─── whenLabel (§6) ──────────────────────────────────────────────────────────

describe('whenLabel', () => {
	it('renders the §6 copy for the two "no focus condition" spellings', () => {
		expect(whenLabel('always')).toBe('always · also in text fields');
		expect(whenLabel('')).toBe('always · also in text fields');
		expect(whenLabel('!inputFocus')).toBe('no focus condition');
	});

	it('passes any other `when` through unchanged', () => {
		expect(whenLabel('filesFocus && resource =~ \'*.ts\'')).toBe('filesFocus && resource =~ \'*.ts\'');
	});
});

// ─── search ──────────────────────────────────────────────────────────────────

describe('matchesQuery / matchesKey', () => {
	it('matchesQuery is a case-insensitive substring match over label, command, key and when', () => {
		const m = model();
		const rows = buildKeyRows(m, 'mac');
		const row = rows.find((r) => r.command === 'explorer.toggle');
		expect(row).toBeDefined();
		if (!row) return;
		expect(matchesQuery(row, '')).toBe(true);
		expect(matchesQuery(row, 'EXPLORER')).toBe(true);
		expect(matchesQuery(row, 'nope-not-here')).toBe(false);
	});

	it('matchesKey compares canonical, platform-resolved sequences, not spelling', () => {
		const m = model();
		const rows = buildKeyRows(m, 'mac');
		const row = rows.find((r) => r.command === 'explorer.toggle');
		expect(row).toBeDefined();
		if (!row) return;
		expect(matchesKey(row, 'mod+b', 'mac')).toBe(true);
		expect(matchesKey(row, 'mod+shift+b', 'mac')).toBe(false);
		const heldRow = { ...row, key: null };
		expect(matchesKey(heldRow, 'mod+b', 'mac')).toBe(false);
	});
});

// ─── conflicts (DEC-59, §5) — never string equality ────────────────────────

describe('conflictsForRow / otherEntry', () => {
	it('finds the clash pair for a row whose key+`when` collides with another command, and names the other side', () => {
		const m = model({
			files: makeFiles({
				personalActions: { version: 1, actions: [userAction('mine', 'personal')] },
				// `mod+b` is `explorer.toggle`'s default, `when: 'always'` — an
				// unqualified personal binding on the same key also reads `always`
				// (§1.5: absent `when` ≡ `always`), so this is a same-key,
				// same-normalized-`when` clash (DEC-59), not precedence.
				personalBindings: [{ key: 'mod+b', command: 'mine' }],
			}),
		});
		const rows = buildKeyRows(m, 'mac');
		const conflicts = m.keymap.conflicts.mac;
		const mine = rows.find((r) => r.command === 'mine');
		expect(mine).toBeDefined();
		if (!mine) return;
		const rc = conflictsForRow(mine, conflicts);
		expect(rc.clashes.length).toBeGreaterThan(0);
		const other = otherEntry(rc.clashes[0], mine.entry!);
		expect(other.command).not.toBe('mine');
	});

	it('a row with no clash returns empty arrays', () => {
		const m = model();
		const rows = buildKeyRows(m, 'mac');
		const row = rows.find((r) => r.command === 'pane.close');
		expect(row).toBeDefined();
		if (!row) return;
		const rc = conflictsForRow(row, m.keymap.conflicts.mac);
		expect(rc.clashes).toEqual([]);
		expect(rc.precedence).toEqual([]);
	});
});
