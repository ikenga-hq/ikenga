// WP-61 — "From a package" import source. Every installed package's own
// DEC-54 key requests (`ui.context_actions[].key`, `ui.command_palette[].shortcut`,
// G-ACTIONS §7/§12) are already live the moment the package is installed —
// there is no "install" step this surface owns, and nothing here creates a
// new `EffectiveAction`. What it *does* let the user do is materialize a
// package's currently-granted request into their own personal
// `keybindings.json`, so the shortcut keeps working even if the package is
// later removed, or a lower-priority grant race changes on the next merge
// (§7.4 "recomputed on every merge"). A request that has instead lost its
// grant race shows here exactly like a VS Code clash: kept yours, imported
// unbound.

import { addKeybinding, keyHolder, type EffectiveModel, type KeybindingRule, type PackageKeyRequest } from '@/lib/actions/store';
import { isMacPlatform } from '@/lib/keymap/platform';
import type { ImportDiffRow } from './vscode-map';

export interface PackageImportRow extends ImportDiffRow {
	/** Set only for `kind: 'add'` — the rule `applyPackageImport` writes. */
	write?: KeybindingRule;
}

function requestLabel(request: PackageKeyRequest, model: EffectiveModel): string {
	return model.actionById.get(request.actionId)?.name ?? request.actionId;
}

/**
 * Classifies every installed package's key request. `skip` here means
 * something narrower than in `vscode.ts` / `project.ts`: this source only
 * ever asks for the *same* action id the key is already granted to (there is
 * no separate id being mapped onto it, unlike a VS Code command translated
 * onto a built-in), so a holder whose command equals the request's own
 * action id is the common, harmless case — the package's own grant (or an
 * earlier run of this same import) already did the job — and is reported as
 * `skip`, not `clash`. A *different* command holding the key is still a
 * clash, same as everywhere else (§5: import never overrides).
 */
export function buildPackageDiff(model: EffectiveModel): PackageImportRow[] {
	const platform = isMacPlatform() ? 'mac' : 'other';
	return model.keymap.packageRequests.map((request): PackageImportRow => {
		const key = `pkg-${request.actionId}`;
		const title = requestLabel(request, model);
		// An invalid request (bad grammar, a chord — §7.1 forbids both) never
		// became a real key on any layer; `request.key` may not even be a
		// well-formed sequence, so this is checked before ever calling
		// `keyHolder` on it (§1.6 `E_KEY_GRAMMAR` would just refuse the write
		// anyway, but there is no point offering a guaranteed-invalid row as
		// "add" in the first place).
		const status = request.byPlatform[platform];
		if (status.status === 'invalid') {
			return { kind: 'skip', key, title, detail: `${request.pkgId} — ${status.reason}` };
		}
		const holder = keyHolder(request.key);
		if (!holder) {
			const rule: KeybindingRule = { key: request.key, command: request.actionId, when: request.when };
			return { kind: 'add', key, title, detail: `${request.pkgId} → your personal keybindings`, write: rule };
		}
		const holderCommand = 'command' in holder ? holder.command : null;
		if (holderCommand === request.actionId) {
			return { kind: 'skip', key, title, detail: `${request.pkgId} — already bound (its own grant, or yours already)` };
		}
		const heldByLabel = holderCommand ?? 'a predefined system shortcut';
		return {
			kind: 'clash',
			key,
			title,
			detail: `${request.pkgId} — already held by \`${heldByLabel}\` — kept yours, imported unbound`,
		};
	});
}

/** Writes every `add` row's rule to `scope`. */
export async function applyPackageImport(rows: readonly PackageImportRow[], scope: 'personal' | 'project'): Promise<void> {
	for (const row of rows) {
		if (row.kind === 'add' && row.write) {
			await addKeybinding(scope, row.write);
		}
	}
}
