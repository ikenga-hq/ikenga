import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
	DEFAULT_TOAST_ACTION_TTL_MS,
	DEFAULT_TOAST_TTL_MS,
	dismissToast,
	MAX_QUEUED_TOASTS,
	toast,
	useToastStore,
} from '@/lib/toast';
import { ToastHost } from './toast-host';

beforeEach(() => {
	vi.useFakeTimers();
	useToastStore.setState({ queue: [] });
});
afterEach(() => {
	cleanup();
	vi.useRealTimers();
});

const tick = (ms: number) =>
	act(() => {
		vi.advanceTimersByTime(ms);
	});

describe('toast() + <ToastHost />', () => {
	it('shows a toast via the mounted host and auto-dismisses at the default ttl', () => {
		render(<ToastHost />);
		expect(screen.queryByRole('status')).toBeNull();
		act(() => {
			toast({ label: 'Saved' });
		});
		expect(screen.getByText('Saved')).toBeTruthy();
		tick(DEFAULT_TOAST_TTL_MS - 1);
		expect(screen.getByText('Saved')).toBeTruthy();
		tick(1);
		expect(screen.queryByText('Saved')).toBeNull();
	});

	it('queues a second toast and shows it after the first goes', () => {
		render(<ToastHost />);
		act(() => {
			toast({ label: 'One', ttlMs: 1000 });
			toast({ label: 'Two', ttlMs: 2000 });
		});
		expect(screen.getByText('One')).toBeTruthy();
		expect(screen.queryByText('Two')).toBeNull();
		tick(1000);
		expect(screen.queryByText('One')).toBeNull();
		expect(screen.getByText('Two')).toBeTruthy();
		tick(1999);
		expect(screen.getByText('Two')).toBeTruthy();
		tick(1);
		expect(screen.queryByText('Two')).toBeNull();
	});

	it('honours ttlMs, and survives host re-renders without restarting', () => {
		const { rerender } = render(<ToastHost />);
		act(() => {
			toast({ label: 'Custom', ttlMs: 800 });
		});
		for (let i = 0; i < 4; i++) {
			tick(150);
			rerender(<ToastHost />);
		}
		tick(199);
		expect(screen.getByText('Custom')).toBeTruthy();
		tick(1);
		expect(screen.queryByText('Custom')).toBeNull();
	});

	it('renders the action, runs it and dismisses on click; actions get a longer default ttl', () => {
		const run = vi.fn();
		render(<ToastHost />);
		act(() => {
			toast({ label: 'Deleted', action: { label: 'Undo', run } });
		});
		tick(DEFAULT_TOAST_TTL_MS + 1);
		expect(screen.getByText('Deleted')).toBeTruthy();
		fireEvent.click(screen.getByRole('button', { name: 'Undo' }));
		expect(run).toHaveBeenCalledTimes(1);
		expect(screen.queryByText('Deleted')).toBeNull();
		act(() => {
			toast({ label: 'Again', action: { label: 'Undo', run } });
		});
		tick(DEFAULT_TOAST_ACTION_TTL_MS);
		expect(screen.queryByText('Again')).toBeNull();
	});

	it('uses the alert role for error toasts and supports dismiss by id / dismissToast()', () => {
		render(<ToastHost />);
		let id = 0;
		act(() => {
			id = toast({ label: 'Boom', variant: 'error' });
			toast({ label: 'Next' });
		});
		expect(screen.getByRole('alert').textContent).toContain('Boom');
		act(() => dismissToast(id));
		expect(screen.getByText('Next')).toBeTruthy();
		act(() => dismissToast());
		expect(screen.queryByText('Next')).toBeNull();
	});

	it('caps the waiting queue, shedding the oldest waiting toast', () => {
		render(<ToastHost />);
		act(() => {
			for (let i = 0; i < MAX_QUEUED_TOASTS + 3; i++) toast({ label: `t${i}` });
		});
		expect(useToastStore.getState().queue).toHaveLength(MAX_QUEUED_TOASTS + 1);
		expect(screen.getByText('t0')).toBeTruthy();
		expect(useToastStore.getState().queue.at(-1)?.label).toBe(`t${MAX_QUEUED_TOASTS + 2}`);
	});
});
