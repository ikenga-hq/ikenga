// WP-55 — the native-menu skeleton (`MENU_TREE`) and its resolver
// (`resolveMenuTree`) render correctly against the *default* effective menus
// (no store started — `getEffectiveMenu` reads `EMPTY_MODEL` outside a
// booted store, so this checks the static skeleton and the frozen
// `DEFAULT_MENUS` data directly, per G-ACTIONS §1.3/§10.4, not a live merge).

import { describe, expect, it } from 'vitest';
import { DEFAULT_MENUS } from '@/lib/actions/menus';
import { isBuiltinActionId } from '@/lib/actions/registry';
import { findEntry } from '@/lib/keymap/registry';
import { cascadeKeyLabel, MENU_TREE, macAccelerator } from './tree';

/** Every canonical action id the default `native/<top>` menus declare
 *  (§10.4) — the ids `resolveMenuTree` would splice into the data-driven
 *  block once a store is running. */
function defaultNativeActionIds(): string[] {
	const ids: string[] = [];
	for (const menu of MENU_TREE) {
		for (const entry of DEFAULT_MENUS[`native/${menu.id}`] ?? []) {
			if (entry !== '---') ids.push(entry.id);
		}
	}
	return ids;
}

describe('MENU_TREE — the OS-role skeleton', () => {
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

	it('every leading/trailing leaf is a predefined OS role, never an action', () => {
		for (const menu of MENU_TREE) {
			for (const entry of [...menu.leading, ...menu.trailing]) {
				if (entry.kind !== 'item') continue;
				expect(entry.predefined, `"${menu.id}/${entry.id}" has no predefined kind`).toBeTruthy();
			}
		}
	});

	it('every leaf id is unique within its own menu (leading + trailing)', () => {
		for (const menu of MENU_TREE) {
			const ids = [...menu.leading, ...menu.trailing].filter((e) => e.kind === 'item').map((e) => e.id);
			expect(new Set(ids).size, `duplicate leaf id in menu "${menu.id}"`).toBe(ids.length);
		}
	});
});

describe('default native/<top> action ids — WP-08 registry parity', () => {
	it('every default action id resolves in the keymap registry or the built-in catalog', () => {
		for (const commandId of defaultNativeActionIds()) {
			expect(
				findEntry(commandId) ?? (isBuiltinActionId(commandId) || undefined),
				`native menu references unknown command "${commandId}"`
			).toBeTruthy();
		}
	});

	it('the DEC-64 leaves carry no accelerator', () => {
		for (const commandId of ['menu.new-session', 'menu.new-terminal', 'session.switch-adapter']) {
			expect(defaultNativeActionIds()).toContain(commandId);
			expect(findEntry(commandId), commandId).toBeUndefined();
			expect(macAccelerator(commandId), commandId).toBeUndefined();
		}
	});

	// WP-46-F0: an action whose only WP-08 registry entry is
	// `platformOnly: 'mac'` must render no key hint on the Windows/Linux
	// cascade — `findEntry`'s permissive same-command fallback (correct for
	// `macAccelerator` / the native macOS menu) must not leak a mac-only
	// binding's key onto a platform where it isn't actually bound.
	it('never shows a key hint on the non-mac cascade for a command whose only registry entry is mac-only', () => {
		for (const commandId of defaultNativeActionIds()) {
			const candidates = findEntry(commandId) ? [findEntry(commandId)!] : [];
			const macOnlyExclusive = candidates.length > 0 && candidates.every((e) => e.platformOnly === 'mac');
			if (!macOnlyExclusive) continue;
			expect(
				cascadeKeyLabel(commandId, { mac: false }),
				`"${commandId}" is mac-only but the non-mac cascade would show a phantom key hint`
			).toBe('');
			expect(cascadeKeyLabel(commandId, { mac: true })).not.toBe('');
		}
	});

	it('still shows the real key hint on the non-mac cascade for a command with a non-mac binding', () => {
		// `ngwa.create` (mod+n) carries no `platformOnly` restriction.
		expect(cascadeKeyLabel('ngwa.create', { mac: false })).toBe('Ctrl+N');
	});
});
