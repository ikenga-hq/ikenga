import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { afterEach, describe, expect, it, vi } from 'vitest';

const listTodos = vi.fn();
vi.mock('@/lib/iyke/memory', () => ({
	listTodos: (...a: unknown[]) => listTodos(...a),
	completeTodo: vi.fn(),
	updateTodo: vi.fn(),
}));
vi.mock('@/lib/panes/pane-store', () => ({
	usePaneStore: { getState: () => ({ focusedId: 'p1', addTab: vi.fn() }) },
}));

import { TodosSection } from './todos';

afterEach(() => {
	cleanup();
	listTodos.mockReset();
});

function renderSection() {
	const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return render(
		<QueryClientProvider client={qc}>
			<TodosSection projectId="p" />
		</QueryClientProvider>
	);
}

describe('TodosSection', () => {
	it('a failed read shows an error row, not "No open todos"', async () => {
		listTodos.mockRejectedValue(new Error('iyke bridge down'));
		renderSection();
		await waitFor(() => expect(screen.getByText("Couldn't load todos")).toBeDefined());
		expect(screen.getByText('iyke bridge down')).toBeDefined();
		expect(screen.queryByText('No open todos')).toBeNull();
	});

	it('an empty list still shows the empty state', async () => {
		listTodos.mockResolvedValue({ todos: [] });
		renderSection();
		await waitFor(() => expect(screen.getByText('No open todos')).toBeDefined());
	});
});
