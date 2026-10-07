import { act, render, renderHook, waitFor } from '@testing-library/react';
import { Pin } from 'lucide-react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

// `virtual:lucide-icon-map` is the shared chunk's one entry point: its factory
// running is the single network request the production build makes. It runs
// again only after `vi.resetModules()`, which is how the tests count it.
const resolveIcon = vi.fn();
const chunkImported = vi.fn();

vi.mock('virtual:lucide-icon-map', () => {
	chunkImported();
	return { resolveIcon: (name: string) => resolveIcon(name) };
});
vi.mock('virtual:lucide-icon-names', () => ({
	default: ['zap', 'git-branch', 'layout-dashboard', 'box'],
}));

const Glyph = Object.assign(() => <svg data-testid="glyph" />, { displayName: 'Glyph' });

beforeEach(() => {
	resolveIcon.mockReset();
	resolveIcon.mockImplementation(() => Glyph);
	chunkImported.mockClear();
});

describe('loadLucideIcon', () => {
	// Fresh module state per test. These tests render nothing, so a second
	// copy of React under the re-imported module is harmless.
	async function fresh() {
		vi.resetModules();
		return await import('./lucide-icons');
	}

	it('imports the shared chunk once for many names', async () => {
		const { loadLucideIcon } = await fresh();
		const icons = await Promise.all([
			loadLucideIcon('zap'),
			loadLucideIcon('git-branch'),
			loadLucideIcon('layout-dashboard'),
			loadLucideIcon('zap'),
		]);
		expect(icons).toEqual([Glyph, Glyph, Glyph, Glyph]);
		await loadLucideIcon('box'); // a later name reuses the loaded chunk
		await loadLucideIcon('git-branch'); // and a resolved one is memoised
		expect(chunkImported).toHaveBeenCalledTimes(1);
		expect(resolveIcon.mock.calls.map((c) => c[0]).sort()).toEqual([
			'box',
			'git-branch',
			'layout-dashboard',
			'zap',
		]);
	});

	it('resolves null for an unknown name without loading the chunk', async () => {
		const { loadLucideIcon, isLucideIconName } = await fresh();
		expect(isLucideIconName('no-such-icon')).toBe(false);
		await expect(loadLucideIcon('no-such-icon')).resolves.toBeNull();
		await expect(loadLucideIcon('constructor')).resolves.toBeNull();
		expect(chunkImported).not.toHaveBeenCalled();
	});

	it('resolves null when the chunk does not know a listed name', async () => {
		const { loadLucideIcon } = await fresh();
		resolveIcon.mockImplementation(() => undefined);
		await expect(loadLucideIcon('box')).resolves.toBeNull();
	});

	it('peek returns nothing until resolved, then the component', async () => {
		const { loadLucideIcon, peekLucideIcon } = await fresh();
		expect(peekLucideIcon('zap')).toBeUndefined();
		const pending = loadLucideIcon('zap');
		expect(peekLucideIcon('zap')).toBeUndefined();
		await pending;
		expect(peekLucideIcon('zap')).toBe(Glyph);
	});
});

describe('useLucideIcon / PinIcon', () => {
	let settle: (() => void) | null = null;

	beforeEach(async () => {
		const { resetLucideIconsForTests } = await import('./lucide-icons');
		resetLucideIconsForTests();
		// Hold resolution so the placeholder state is observable.
		resolveIcon.mockImplementation(
			() =>
				new Promise((r) => {
					settle = () => r(Glyph);
				})
		);
	});

	it('is null (so callers show their placeholder) before resolve, the icon after', async () => {
		const { useLucideIcon } = await import('./lucide-icons');
		const { result } = renderHook(() => useLucideIcon('zap'));
		expect(result.current).toBeNull();
		await waitFor(() => expect(settle).not.toBeNull());
		await act(async () => settle?.());
		await waitFor(() => expect(result.current).toBe(Glyph));
	});

	it('never loads the chunk for an unknown or empty name', async () => {
		chunkImported.mockClear();
		const { useLucideIcon } = await import('./lucide-icons');
		const a = renderHook(() => useLucideIcon('no-such-icon'));
		const b = renderHook(() => useLucideIcon(null));
		expect(a.result.current).toBeNull();
		expect(b.result.current).toBeNull();
		expect(resolveIcon).not.toHaveBeenCalled();
	});

	it('PinIcon shows the fallback glyph, then swaps in the resolved icon', async () => {
		const { PinIcon } = await import('@/shell/pin-icon');
		const { container, queryByTestId } = render(
			<PinIcon iconLucide="LayoutDashboard" iconEmoji={null} Fallback={Pin} />
		);
		expect(container.querySelector('svg.lucide-pin')).not.toBeNull();
		expect(queryByTestId('glyph')).toBeNull();
		await waitFor(() => expect(settle).not.toBeNull());
		await act(async () => settle?.());
		await waitFor(() => expect(queryByTestId('glyph')).not.toBeNull());
		expect(container.querySelector('svg.lucide-pin')).toBeNull();
	});
});
