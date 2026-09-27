// WP-63 (01 §Phase 6 verification, Menus): the menu a user opens is exactly
// the effective data. For each Explorer menu, right-clicking an
// `EffectiveContextMenu` trigger renders the same items, in the same order,
// as `resolveMenuItems(getEffectiveMenu(id))`. The WP-04 stub arrays are gone,
// so this is the only definition of an Explorer menu's contents.

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { getEffectiveMenu } from '@/lib/actions/store';
import { EffectiveContextMenu } from './effective-context-menu';
import { resolveMenuItems } from './resolve';

afterEach(cleanup);

const EXPLORER_MENUS = [
	'files',
	'artifacts',
	'session',
	'automations',
	'ngwa-project',
	'scratchpads',
	'todos',
	'views',
];

/** Every item applicable and handled, so nothing is skipped for want of
 *  context — the comparison is data vs render, not the A-9 / condition rules
 *  (those have their own tests in `resolve.test.ts`). */
function openOptions(menuId: string) {
	const items = (getEffectiveMenu(menuId)?.items ?? []).flatMap((item) =>
		item.kind === 'separator' ? [] : [item]
	);
	const handlers = Object.fromEntries(items.map((item) => [item.id, () => {}]));
	const conditions = Object.fromEntries(
		items.flatMap((item) => (item.condition ? [[item.condition, true]] : []))
	);
	return { handlers, conditions };
}

describe('Explorer context menus render the effective model', () => {
	for (const menuId of EXPLORER_MENUS) {
		it(`"${menuId}": rendered items equal resolveMenuItems(getEffectiveMenu("${menuId}"))`, () => {
			const opts = openOptions(menuId);
			const expected = resolveMenuItems(getEffectiveMenu(menuId), opts).flatMap((row) =>
				row.kind === 'separator' ? [] : [row.label]
			);
			expect(expected.length, `menu "${menuId}" resolves to no items`).toBeGreaterThan(0);

			render(
				<EffectiveContextMenu menuId={menuId} {...opts}>
					<div data-testid="row">row</div>
				</EffectiveContextMenu>
			);
			fireEvent.contextMenu(screen.getByTestId('row'));

			const rendered = screen.getAllByRole('menuitem').map((el) => el.textContent ?? '');
			expect(rendered).toHaveLength(expected.length);
			rendered.forEach((text, i) => {
				expect(
					text.startsWith(expected[i]),
					`item ${i} of "${menuId}": "${text}" vs "${expected[i]}"`
				).toBe(true);
			});
		});
	}
});
