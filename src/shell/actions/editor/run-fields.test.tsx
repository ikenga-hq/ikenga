// WP-71a — the Run section's seat input (G-SEATS §9.1): a `Seat` target in
// the Chi target segment, and, while it is chosen, a field bound to the
// form's `chiSeat` with the active project's seats offered as chips. Pure
// props in, callbacks out: no store is touched. Written under DEC-50: not
// run here.

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { ChiTarget } from '@/lib/actions/client';
import { RunFields, type RunFieldsProps } from './run-fields';

function props(over: Partial<RunFieldsProps> = {}): RunFieldsProps {
	return {
		runType: 'chi',
		onChangeRunType: vi.fn(),
		chiTarget: 'active',
		onChangeChiTarget: vi.fn(),
		chiEngineId: '',
		onChangeChiEngineId: vi.fn(),
		chiSeat: '',
		onChangeChiSeat: vi.fn(),
		seatSuggestions: [],
		chiPrompt: 'x',
		onChangeChiPrompt: vi.fn(),
		shellCommand: '',
		onChangeShellCommand: vi.fn(),
		shellCwd: '',
		onChangeShellCwd: vi.fn(),
		shellConfirm: false,
		onChangeShellConfirm: vi.fn(),
		iykeRoute: '',
		onChangeIykeRoute: vi.fn(),
		iykeMethod: 'POST',
		onChangeIykeMethod: vi.fn(),
		skillName: '',
		onChangeSkillName: vi.fn(),
		workflowName: '',
		onChangeWorkflowName: vi.fn(),
		openUrl: '',
		onChangeOpenUrl: vi.fn(),
		...over,
	};
}

afterEach(() => {
	cleanup();
});

describe('RunFields — seat target', () => {
	it('offers Seat in the Chi target segment, after D-06’s three', () => {
		const onChangeChiTarget = vi.fn<(t: ChiTarget) => void>();
		render(<RunFields {...props({ onChangeChiTarget })} />);
		const seg = screen.getByRole('group', { name: 'Chi target' });
		const labels = Array.from(seg.querySelectorAll('button')).map((b) => b.textContent);
		expect(labels).toEqual(['Active session', 'New session', 'Pick engine', 'Seat']);
		fireEvent.click(screen.getByRole('button', { name: 'Seat' }));
		expect(onChangeChiTarget).toHaveBeenCalledWith('seat');
	});

	it('shows no seat field for any other target', () => {
		render(
			<RunFields {...props({ chiTarget: 'engine', chiSeat: 'lead', seatSuggestions: ['lead'] })} />
		);
		expect(screen.queryByRole('textbox', { name: 'Seat' })).toBeNull();
		expect(screen.queryByRole('group', { name: 'Seats in this project' })).toBeNull();
	});

	it('binds the field to chiSeat and accepts free text (`<project>/<name>`)', () => {
		const onChangeChiSeat = vi.fn<(v: string) => void>();
		render(<RunFields {...props({ chiTarget: 'seat', chiSeat: 'lead', onChangeChiSeat })} />);
		const field = screen.getByRole('textbox', { name: 'Seat' }) as HTMLInputElement;
		expect(field.value).toBe('lead');
		// The Editor's own field style (D-06 `.field`), as the engine field uses.
		expect(field.parentElement?.className).toBe('field');
		fireEvent.change(field, { target: { value: 'royalti-co/lead' } });
		expect(onChangeChiSeat).toHaveBeenCalledWith('royalti-co/lead');
	});

	it('offers the active project’s seats as chips that fill the field', () => {
		const onChangeChiSeat = vi.fn<(v: string) => void>();
		render(
			<RunFields
				{...props({
					chiTarget: 'seat',
					chiSeat: 'lead',
					seatSuggestions: ['lead', 'review'],
					onChangeChiSeat,
				})}
			/>
		);
		const chips = screen.getByRole('group', { name: 'Seats in this project' });
		const lead = screen.getByRole('button', { name: '@lead' });
		const review = screen.getByRole('button', { name: '@review' });
		expect(chips.contains(lead)).toBe(true);
		expect(lead.getAttribute('aria-pressed')).toBe('true');
		expect(review.getAttribute('aria-pressed')).toBe('false');
		fireEvent.click(review);
		expect(onChangeChiSeat).toHaveBeenCalledWith('review');
	});

	it('shows no chip row when the roster is empty or not loaded', () => {
		render(<RunFields {...props({ chiTarget: 'seat' })} />);
		expect(screen.getByRole('textbox', { name: 'Seat' })).toBeTruthy();
		expect(screen.queryByRole('group', { name: 'Seats in this project' })).toBeNull();
	});
});
