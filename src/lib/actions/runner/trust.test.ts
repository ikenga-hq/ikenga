import { afterEach, describe, expect, it, vi } from 'vitest';

const { invokeMock, listenMock } = vi.hoisted(() => ({
	invokeMock: vi.fn(),
	listenMock: vi.fn(),
}));

vi.mock('@/lib/tauri-cmd', () => ({
	invoke: invokeMock,
	listen: listenMock,
}));

import type { ActionRun, ActionsScope, ActionsTrustStatus, ActionTrust } from '../types';
import {
	bindingsHash,
	canonicalJson,
	checkActionTrust,
	isTrustGated,
	runHash,
	sha256HexSync,
	trustShown,
	untrustedActions,
} from './trust';

const SHELL: ActionRun = { kind: 'shell', command: 'echo hi', confirm: true };
// sha256('{"command":"echo hi","confirm":true,"kind":"shell"}') — the bytes
// `src-tauri/src/actions/trust.rs::run_hash` hashes for the same run.
const SHELL_HASH = '36502fcca706c33b913640cf20355ca499a8aee918c9fd0109589563ef25fb74';

function status(actions: ActionTrust[]): ActionsTrustStatus {
	return {
		projectId: 'p1',
		projectRoot: '/p',
		actions,
		actionsError: null,
		actionsStale: false,
		keybindings: { hash: null, ruleCount: 0, state: 'absent' },
		keybindingsError: null,
		keybindingsStale: false,
	};
}

function entry(id: string, run: ActionRun, hash: string, state: ActionTrust['state']): ActionTrust {
	return { id, name: id, kind: run.kind, run, hash, state };
}

afterEach(() => {
	vi.clearAllMocks();
});

describe('canonical JSON + hashes (B-14, B-26)', () => {
	it('sorts keys at every level, no whitespace', () => {
		expect(canonicalJson({ b: 1, a: { d: [1, { z: 'x', y: null }], c: true } })).toBe(
			'{"a":{"c":true,"d":[1,{"y":null,"z":"x"}]},"b":1}'
		);
	});

	it('matches the Rust run hash', async () => {
		expect(await runHash(SHELL)).toBe(SHELL_HASH);
		expect(await runHash({ confirm: true, kind: 'shell', command: 'echo hi' } as ActionRun)).toBe(SHELL_HASH);
	});

	it('the plain SHA-256 fallback agrees with the digest', () => {
		const bytes = new TextEncoder().encode('{"command":"echo hi","confirm":true,"kind":"shell"}');
		expect(sha256HexSync(bytes)).toBe(SHELL_HASH);
		expect(sha256HexSync(new Uint8Array())).toBe(
			'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'
		);
		// Crosses a 64-byte block boundary (padding into a second block).
		expect(sha256HexSync(new TextEncoder().encode('a'.repeat(56)))).toBe(
			'b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a'
		);
	});

	it('matches the Rust bindings hash', async () => {
		expect(await bindingsHash([{ key: 'mod+w', command: '-pane.close' }])).toBe(
			'9c5ad4cb3b8f65b1e697aabb28e86d7bf6e9d2f815c8ea92e5cb12994d8a40fb'
		);
		expect(await bindingsHash(undefined)).toBe('4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945');
	});
});

describe('checkActionTrust (DEC-55)', () => {
	it('gates only project shell / iyke / skill / workflow', () => {
		for (const kind of ['shell', 'iyke', 'skill', 'workflow'] as const) {
			expect(isTrustGated('project', kind)).toBe(true);
			expect(isTrustGated('personal', kind)).toBe(false);
		}
		expect(isTrustGated('project', 'chi')).toBe(false);
		expect(isTrustGated('project', 'open')).toBe(false);
	});

	it('fails closed on an unexpected scope', () => {
		const odd = 'package' as unknown as ActionsScope;
		expect(isTrustGated(odd, 'shell')).toBe(true);
		expect(isTrustGated(undefined as unknown as ActionsScope, 'iyke')).toBe(true);
	});

	it('does not read trust for an ungated run', async () => {
		const read = vi.fn();
		expect(await checkActionTrust({ id: 'a', run: SHELL, scope: 'personal' }, 'p1', read)).toEqual({
			ok: true,
			gated: false,
			hash: null,
		});
		await checkActionTrust({ id: 'a', run: { kind: 'open', url: '/x' }, scope: 'project' }, 'p1', read);
		expect(read).not.toHaveBeenCalled();
	});

	it('is fail-closed: an id missing from the status is untrusted', async () => {
		const read = vi.fn().mockResolvedValue(status([]));
		const check = await checkActionTrust({ id: 'a', run: SHELL, scope: 'project' }, 'p1', read);
		expect(check).toMatchObject({ ok: false, reason: 'untrusted' });
		expect(read).toHaveBeenCalledWith('p1');
	});

	it('a failed status read refuses', async () => {
		const read = vi.fn().mockRejectedValue(new Error('no project root'));
		expect(await checkActionTrust({ id: 'a', run: SHELL, scope: 'project' }, 'p1', read)).toMatchObject({
			ok: false,
			reason: 'trust-unavailable',
		});
	});

	it('runs only when trusted at the same hash', async () => {
		const trusted = vi.fn().mockResolvedValue(status([entry('a', SHELL, SHELL_HASH, 'trusted')]));
		expect(await checkActionTrust({ id: 'a', run: SHELL, scope: 'project' }, 'p1', trusted)).toEqual({
			ok: true,
			gated: true,
			hash: SHELL_HASH,
		});

		const untrusted = vi.fn().mockResolvedValue(status([entry('a', SHELL, SHELL_HASH, 'untrusted')]));
		expect(await checkActionTrust({ id: 'a', run: SHELL, scope: 'project' }, 'p1', untrusted)).toMatchObject({
			ok: false,
			reason: 'untrusted',
		});

		const changed = vi.fn().mockResolvedValue(status([entry('a', SHELL, SHELL_HASH, 'changed')]));
		expect(await checkActionTrust({ id: 'a', run: SHELL, scope: 'project' }, 'p1', changed)).toMatchObject({
			ok: false,
			reason: 'changed',
		});
	});

	it('editing a trusted command re-asks (the held run no longer hashes to the pin)', async () => {
		const read = vi.fn().mockResolvedValue(status([entry('a', SHELL, SHELL_HASH, 'trusted')]));
		const edited: ActionRun = { kind: 'shell', command: 'echo pwned', confirm: true };
		expect(await checkActionTrust({ id: 'a', run: edited, scope: 'project' }, 'p1', read)).toMatchObject({
			ok: false,
			reason: 'changed',
		});
	});
});

describe('trust sheet helpers', () => {
	it('lists untrusted and changed entries only', () => {
		const list = untrustedActions(
			status([
				entry('a', SHELL, 'h', 'trusted'),
				entry('b', SHELL, 'h', 'untrusted'),
				entry('c', SHELL, 'h', 'changed'),
				entry('d', { kind: 'chi', target: 'new', prompt: 'x' }, 'h', 'not-gated'),
			])
		).map((e) => e.id);
		expect(list).toEqual(['b', 'c']);
	});

	it('grants exactly the shown hashes, keybindings included (DEC-65)', async () => {
		invokeMock.mockResolvedValue(status([]));
		await trustShown('p1', { actions: [entry('b', SHELL, 'hb', 'untrusted')], keybindingsHash: 'kh' });
		expect(invokeMock).toHaveBeenCalledWith('actions_trust_grant', {
			request: { actions: [{ id: 'b', hash: 'hb' }], keybindings: 'kh' },
			projectId: 'p1',
		});
	});
});
