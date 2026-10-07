import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { CorruptCustomShellsError } from '@/lib/shell-profiles';
import { CustomShellsStatus } from './custom-shells-status';

afterEach(cleanup);

const base = {
	error: null,
	isCorrupt: false,
	onReset: vi.fn().mockResolvedValue('k'),
	onRetry: vi.fn(),
	isResetting: false,
	resetError: null,
	resetBackupKey: null,
};

describe('CustomShellsStatus (D-13)', () => {
	it('a corrupt value offers Reset custom shells, which calls the reset', () => {
		const onReset = vi.fn().mockResolvedValue('terminal.custom_shell_profiles.corrupt-1');
		render(
			<CustomShellsStatus
				{...base}
				error={new CorruptCustomShellsError('saved custom shell profiles are not a list', '{}')}
				isCorrupt
				onReset={onReset}
			/>
		);
		expect(screen.getByTestId('custom-shells-read-error')).toBeDefined();
		fireEvent.click(screen.getByTestId('custom-shells-reset'));
		expect(onReset).toHaveBeenCalledTimes(1);
	});

	it('a temporary read failure shows the error but no reset button', () => {
		render(<CustomShellsStatus {...base} error={new Error('ipc timeout')} />);
		expect(screen.getByText(/ipc timeout/)).toBeDefined();
		expect(screen.queryByTestId('custom-shells-reset')).toBeNull();
		fireEvent.click(screen.getByText('Retry'));
		expect(base.onRetry).toHaveBeenCalled();
	});

	it('a failed reset says why and that nothing was cleared', () => {
		render(
			<CustomShellsStatus
				{...base}
				error={new CorruptCustomShellsError('not valid JSON', '{bad')}
				isCorrupt
				resetError={new Error('disk full')}
			/>
		);
		expect(screen.getByText(/disk full/)).toBeDefined();
		expect(screen.getByText(/were not cleared/)).toBeDefined();
	});

	it('after a reset it names the backup setting', () => {
		render(
			<CustomShellsStatus {...base} resetBackupKey="terminal.custom_shell_profiles.corrupt-7" />
		);
		expect(screen.getByTestId('custom-shells-reset-done').textContent).toContain(
			'terminal.custom_shell_profiles.corrupt-7'
		);
		expect(screen.queryByTestId('custom-shells-read-error')).toBeNull();
	});
});
