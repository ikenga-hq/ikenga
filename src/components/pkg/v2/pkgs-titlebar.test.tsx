// Render tests for PkgsTitlebar. Cheap to render — no Tauri, no queries,
// just a presentational strip. We're really testing the search-input
// behaviour wired through `onQueryChange`, and that the install pkg
// button surfaces the count strings sanely.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

afterEach(cleanup);

import { PkgsTitlebar } from './pkgs-titlebar';
import type { DerivedPkgs } from '@/lib/pkgs/use-derived';

function makeDerived(overrides: Partial<DerivedPkgs> = {}): DerivedPkgs {
	return {
		rows: [],
		installed: [],
		registry: [],
		updates: [],
		trust: [],
		violations: [],
		builtin: [],
		engine: [],
		user: [],
		sidecarsRunning: 0,
		isLoading: false,
		error: null,
		trustUnavailable: null,
		...overrides,
	};
}

describe('PkgsTitlebar', () => {
	it('renders the title and singular sidecar phrasing', () => {
		render(
			<PkgsTitlebar
				d={makeDerived({
					installed: [{} as never],
					sidecarsRunning: 1,
				})}
				onInstallPkg={() => {}}
			/>
		);
		expect(screen.getByText('Packages')).toBeTruthy();
		expect(screen.getByText(/1 installed/)).toBeTruthy();
		expect(screen.getByText(/1 sidecar running/)).toBeTruthy();
	});

	it('uses plural "sidecars" when the count is 0 or >1', () => {
		const { rerender } = render(
			<PkgsTitlebar d={makeDerived({ sidecarsRunning: 0 })} onInstallPkg={() => {}} />
		);
		expect(screen.getByText(/0 sidecars running/)).toBeTruthy();
		rerender(<PkgsTitlebar d={makeDerived({ sidecarsRunning: 3 })} onInstallPkg={() => {}} />);
		expect(screen.getByText(/3 sidecars running/)).toBeTruthy();
	});

	it('omits the search input when onQueryChange is not provided', () => {
		render(<PkgsTitlebar d={makeDerived()} onInstallPkg={() => {}} />);
		expect(screen.queryByPlaceholderText(/Filter by name/i)).toBeNull();
	});

	it('emits onQueryChange while typing', async () => {
		const user = userEvent.setup();
		const onQueryChange = vi.fn();
		render(
			<PkgsTitlebar
				d={makeDerived()}
				query=""
				onQueryChange={onQueryChange}
				onInstallPkg={() => {}}
			/>
		);
		const input = screen.getByPlaceholderText(/Filter by name/i) as HTMLInputElement;
		await user.type(input, 'iyke');
		// userEvent fires one change per keystroke
		expect(onQueryChange).toHaveBeenCalledTimes(4);
		expect(onQueryChange).toHaveBeenNthCalledWith(1, 'i');
		expect(onQueryChange).toHaveBeenNthCalledWith(4, 'e');
	});

	it('fires onInstallPkg when the [Install pkg] button is clicked', async () => {
		const user = userEvent.setup();
		const onInstallPkg = vi.fn();
		render(<PkgsTitlebar d={makeDerived()} onInstallPkg={onInstallPkg} />);
		await user.click(screen.getByRole('button', { name: /Install pkg/i }));
		expect(onInstallPkg).toHaveBeenCalledTimes(1);
	});
});
