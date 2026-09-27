import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';

import { useShellStore } from '@/lib/shell/shell-store';
import { Sidebar } from './sidebar';

vi.mock('@/lib/shell/shell-store');
vi.mock('./explorer/explorer', () => ({
	Explorer: ({ only }: { only?: string }) => <div data-testid="explorer" data-only={only ?? ''} />,
}));

function renderIn(activeMode: string) {
	(useShellStore as unknown as ReturnType<typeof vi.fn>).mockImplementation(
		(selector: (s: { activeMode: string }) => unknown) => selector({ activeMode })
	);
	render(<Sidebar />);
	return screen.getByTestId('explorer').getAttribute('data-only');
}

describe('Sidebar body per rail mode (D-01, D-03)', () => {
	afterEach(cleanup);

	it('Project shows the whole Explorer', () => {
		expect(renderIn('project')).toBe('');
	});

	it('Chi shows the Sessions section and Ngwa the project equipment', () => {
		expect(renderIn('chi')).toBe('sessions');
		cleanup();
		expect(renderIn('ngwa')).toBe('ngwa-project');
	});

	it('Settings shows the Explorer, not a second copy of the section nav', () => {
		expect(renderIn('settings')).toBe('');
		expect(screen.queryByText('Appearance')).toBeNull();
	});
});
