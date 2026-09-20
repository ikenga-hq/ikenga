import { useCallback } from 'react';
import { CheckSquare, Square } from 'lucide-react';
import { useQuery } from '@tanstack/react-query';
import { ListRow } from '@/components/ui/list-row';
import { usePaneStore } from '@/lib/panes/pane-store';
import { listTodos, type Todo } from '@/lib/iyke/memory';
import type { ExplorerSectionContext } from '../section-registry';

export const todosContextMenu = [
	{ id: 'toggle-done', label: 'Toggle done', run: () => {} },
	{ id: 'open-source', label: 'Open source file', run: () => {} },
	{ id: 'hand-to-chi', label: 'Hand to Chi', run: () => {} },
];

export function TodosSection({ projectId }: ExplorerSectionContext) {
	const query = useQuery<Todo[]>({
		queryKey: ['explorer-todos', projectId],
		queryFn: async () => {
			try {
				const res = await listTodos({ scope: `project:${projectId}` });
				return res?.todos?.filter((t) => t.status !== 'done') ?? [];
			} catch {
				return [];
			}
		},
		staleTime: 15_000,
	});

	const todos = query.data ?? [];

	const openTodos = useCallback(() => {
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'route', path: '/todos' });
	}, []);

	if (todos.length === 0) {
		return (
			<div className="p-4 text-center">
				<h3 className="text-sm font-semibold">No open todos</h3>
				<p className="text-xs text-muted-foreground mt-1 mb-3">
					Track tasks and todos scoped to this project.
				</p>
				<button
					type="button"
					onClick={openTodos}
					className="text-xs bg-primary text-primary-foreground px-3 py-1.5 rounded hover:bg-primary/90 transition-colors"
				>
					Open Todos
				</button>
			</div>
		);
	}

	return (
		<div className="py-1">
			{todos.map((todo) => (
				<ListRow
					key={todo.id}
					size="sm"
					onActivate={openTodos}
					title={todo.title}
					className="w-full gap-1.5 px-2"
				>
					{todo.status === 'done' ? (
						<CheckSquare className="h-3.5 w-3.5 shrink-0 text-emerald-500" />
					) : (
						<Square className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
					)}
					<span className="flex-1 truncate text-xs">{todo.title}</span>
				</ListRow>
			))}
		</div>
	);
}
