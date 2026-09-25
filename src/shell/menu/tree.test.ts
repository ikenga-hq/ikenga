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
import { findEntry } from '@/lib/keymap/registry';
import { allMenuCommandIds, MENU_TREE } from './tree';

describe('MENU_TREE — WP-08 registry parity', () => {
	it('every commandId used by a menu item resolves in the keymap registry', () => {
		for (const commandId of allMenuCommandIds()) {
			expect(findEntry(commandId), `menu item references unknown command "${commandId}"`).toBeTruthy();
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
});
