// WP-46 self-verifiable DoD: every `MENU_TREE` leaf that claims a WP-08
// registry command id actually resolves one — a menu item's shown key can
// never silently go blank because of a typo'd id. Also proves the tree
// itself is well-formed (unique ids, no duplicate top-level menus) and that
// the omission rule ("existing command or omitted") holds: every leaf either
// has a `commandId`, an `action`/`predefined`, or is deliberately
// structure-only per its own comment in `tree.tsx` — this test doesn't
// enforce that last case (it's a documented, reviewed decision per item),
// but it does prove nothing *claims* a command id that doesn't exist.

import { describe, expect, it } from 'vitest';
import { isBuiltinActionId } from '@/lib/actions/registry';
import { findEntry, getKeymap } from '@/lib/keymap/registry';
import { allMenuCommandIds, cascadeKeyLabel, MENU_TREE, macAccelerator } from './tree';

describe('MENU_TREE — WP-08 registry parity', () => {
	it('every commandId used by a menu item resolves in the keymap registry or the built-in catalog', () => {
		// DEC-64 / DEC-63.2 (WP-54) left three leaves deliberately unbound
		// (`menu.new-session`, `menu.new-terminal`, `session.switch-adapter`):
		// no keymap entry, still a built-in action, rendered unaccelerated.
		for (const commandId of allMenuCommandIds()) {
			expect(
				findEntry(commandId) ?? (isBuiltinActionId(commandId) || undefined),
				`menu item references unknown command "${commandId}"`
			).toBeTruthy();
		}
	});

	it('the DEC-64 leaves carry no accelerator', () => {
		for (const commandId of ['menu.new-session', 'menu.new-terminal', 'session.switch-adapter']) {
			expect(allMenuCommandIds()).toContain(commandId);
			expect(findEntry(commandId), commandId).toBeUndefined();
			expect(macAccelerator(commandId), commandId).toBeUndefined();
		}
	});

	it('has all nine D-08 menus, in order, each with a unique id', () => {
		expect(MENU_TREE.map((m) => m.id)).toEqual([
			'ikenga',
			'file',
			'edit',
			'view',
			'project',
			'chi',
			'ngwa',
			'window',
			'help',
		]);
	});

	it('every leaf id is unique within its own menu', () => {
		for (const menu of MENU_TREE) {
			const ids = menu.items.filter((e) => e.kind === 'item').map((e) => e.id);
			expect(new Set(ids).size, `duplicate leaf id in menu "${menu.id}"`).toBe(ids.length);
		}
	});

	it('no leaf both claims a commandId and is macOnly-predefined without a fallback kind', () => {
		for (const menu of MENU_TREE) {
			for (const entry of menu.items) {
				if (entry.kind !== 'item') continue;
				// A leaf with `predefined` never also carries `commandId` — the
				// two renderers pick one path or the other, never both.
				if (entry.predefined) {
					expect(entry.commandId, `"${entry.id}" is predefined but also claims a commandId`).toBeUndefined();
				}
			}
		}
	});

	// WP-46-F0: a menu leaf whose only WP-08 registry entry is
	// `platformOnly: 'mac'` must render no key hint on the Windows/Linux
	// cascade — `findEntry`'s permissive same-command fallback (correct for
	// `macAccelerator` / the native macOS menu) must not leak a mac-only
	// binding's key onto a platform where it isn't actually bound.
	it('never shows a key hint on the non-mac cascade for a command whose only registry entry is mac-only', () => {
		for (const commandId of allMenuCommandIds()) {
			const candidates = getKeymap().filter((e) => e.command === commandId);
			const macOnlyExclusive =
				candidates.length > 0 && candidates.every((e) => e.platformOnly === 'mac');
			if (!macOnlyExclusive) continue;
			expect(
				cascadeKeyLabel(commandId, { mac: false }),
				`"${commandId}" is mac-only but the non-mac cascade would show a phantom key hint`
			).toBe('');
			// The same command still shows its real hint on macOS.
			expect(cascadeKeyLabel(commandId, { mac: true })).not.toBe('');
		}
	});

	it('still shows the real key hint on the non-mac cascade for a command with a non-mac binding', () => {
		// `ngwa.create` (mod+n) carries no `platformOnly` restriction — it's
		// the live, working Ctrl+N binding the mac-only `menu.new-session`
		// must not be confused for (WP-46-F0's own regression example).
		expect(cascadeKeyLabel('ngwa.create', { mac: false })).toBe('Ctrl+N');
	});
});
