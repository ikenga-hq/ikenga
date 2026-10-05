// WP-76: the D-03 Secrets principal axis on remote (over remote-access
// WP-21's per-principal store; G-ACCESS §10.2).

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import {
	hasPassphraseLayer,
	OPERATOR_SCOPE_REASON,
	PrincipalAxis,
	SHARED_REASON,
	scopeDisabledReason,
	secretLayer,
	settledLayerNames,
	vaultAxis,
} from './principal-axis';

afterEach(cleanup);

const share = {
	projectKey: '01890a5d-ac96-774b-bcce-b302099a8057/royalti-co',
	projectId: 'royalti-co',
	projectName: 'royalti-co',
	ownerUsername: 'ada',
	role: 'operator' as const,
	scope: 'project' as const,
};

describe('principal axis', () => {
	it('picks the vault from the transport and the store mode', () => {
		expect(vaultAxis({ remote: false, mode: 'keychain', share: null })).toBe('desktop');
		expect(vaultAxis({ remote: true, mode: 'principal', share: null })).toBe('principal');
		expect(vaultAxis({ remote: true, mode: 'env', share: null })).toBe('operator');
		expect(vaultAxis({ remote: true, mode: 'principal', share })).toBe('shared');
	});

	it('scopes: all three on your own store, workspace only on the operator default, none in a share', () => {
		for (const s of ['workspace', 'project', 'pkg'] as const) {
			expect(scopeDisabledReason('principal', s)).toBeNull();
			expect(scopeDisabledReason('shared', s)).toBe(SHARED_REASON);
		}
		expect(scopeDisabledReason('operator', 'workspace')).toBeNull();
		expect(scopeDisabledReason('operator', 'project')).toBe(OPERATOR_SCOPE_REASON);
		expect(hasPassphraseLayer('desktop')).toBe(true);
		expect(hasPassphraseLayer('principal')).toBe(false);
	});

	it('renders whose secrets these are', () => {
		const { container, rerender } = render(
			<PrincipalAxis axis="principal" username="ada" share={null} />
		);
		expect(screen.getByText("ada's store")).toBeTruthy();
		expect(screen.getByText('Operator default')).toBeTruthy();
		expect(container.querySelector('[data-state="secrets-principal"]')).toBeTruthy();
		rerender(<PrincipalAxis axis="shared" username={null} share={share} />);
		expect(screen.getByText(SHARED_REASON)).toBeTruthy();
		rerender(<PrincipalAxis axis="desktop" username={null} share={null} />);
		expect(container.textContent).toBe('');
	});

	it('tells your keys from the operator default in a mixed Workspace list (WP76-R3)', () => {
		// `secrets_index_names`: env names bare, store names `workspace::KEY`.
		const index = ['OPENAI_KEY', 'SHARED', 'workspace::MINE', 'workspace::SHARED', 'project::p::X'];
		const layer = (k: string) => secretLayer('principal', 'workspace', k, index);
		expect(layer('MINE')).toBe('own');
		expect(layer('SHARED')).toBe('override');
		expect(layer('OPENAI_KEY')).toBe('default');
		// Unknown layers are never presented as yours.
		expect(secretLayer('principal', 'workspace', 'MINE', undefined)).toBe('default');
		// Project / pkg scopes exist only in your store; the T0 default is all default.
		expect(secretLayer('principal', 'project', 'X', index)).toBe('own');
		expect(secretLayer('operator', 'workspace', 'OPENAI_KEY', index)).toBe('default');
		expect(secretLayer('desktop', 'workspace', 'ANY', undefined)).toBe('own');
	});

	it('rows read as default until the default list settles (WP78a-R6)', () => {
		const index = ['MINE', 'workspace::MINE'];
		const layer = (d: { data?: readonly string[]; isError: boolean }) => {
			const n = settledLayerNames(index, d);
			return secretLayer('principal', 'workspace', 'MINE', n.indexNames, n.defaultNames);
		};
		// Loading: never a flash of "override" (nor of "own").
		expect(layer({ isError: false })).toBe('default');
		expect(layer({ data: [], isError: false })).toBe('own');
		// Failed (an older daemon): the index's bare names decide.
		expect(layer({ isError: true })).toBe('override');
	});

	it('a bare key in your own store is not "your override" (WP76-RV1)', () => {
		// MINE is in your store twice (bare and `workspace::`); no operator
		// default holds it. SHARED really overrides one.
		const index = ['MINE', 'OPENAI_KEY', 'SHARED', 'workspace::MINE', 'workspace::SHARED'];
		const defaults = ['OPENAI_KEY', 'SHARED'];
		const layer = (k: string) => secretLayer('principal', 'workspace', k, index, defaults);
		expect(layer('MINE')).toBe('own');
		expect(layer('SHARED')).toBe('override');
		expect(layer('OPENAI_KEY')).toBe('default');
		// Without the default list (an older daemon) the index decides.
		expect(secretLayer('principal', 'workspace', 'MINE', index)).toBe('override');
	});
});
