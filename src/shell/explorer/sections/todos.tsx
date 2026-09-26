import { useCallback } from 'react';
import { CheckSquare, Square } from 'lucide-react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { ListRow } from '@/components/ui/list-row';
import { usePaneStore } from '@/lib/panes/pane-store';
import { completeTodo, listTodos, updateTodo, type Todo } from '@/lib/iyke/memory';
import { EffectiveContextMenu } from '@/shell/menu/effective-context-menu';
import { handToChi } from '@/shell/companion/companion-store';
import type { ExplorerSectionContext } from '../section-registry';

// WP-04 stub array — real menu content is `getEffectiveMenu('todos')` below
// (G-ACTIONS §1.3). Kept for `section-registry.ts`'s unused `contextMenu`
// field (out of this WP's FILES list; see the PR report).
export const todosContextMenu = [
	{ id: 'toggle-done', label: 'Toggle done', run: () => {} },
	{ id: 'open-source', label: 'Open source file', run: () => {} },
	{ id: 'hand-to-chi', label: 'Hand to Chi', run: () => {} },
];

export function TodosSection({ projectId }: ExplorerSectionContext) {
	const qc = useQueryClient();
	const queryKey = ['explorer-todos', projectId];
	const query = useQuery<Todo[]>({
		queryKey,
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

	const toggleDone = useCallback(
		async (todo: Todo) => {
			try {
				if (todo.status === 'done') await updateTodo({ id: todo.id, status: 'open' });
				else await completeTodo(todo.id);
			} finally {
				void qc.invalidateQueries({ queryKey });
			}
		},
		[qc, queryKey]
	);

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
				<EffectiveContextMenu
					key={todo.id}
					menuId="todos"
					// A-9: `open-source` is left out — todos carry no source-file
					// reference, so it could only open the Todos page.
					builtinsNeedHandler
					handlers={{
						'toggle-done': () => void toggleDone(todo),
						'hand-to-chi': () => handToChi(todo.title),
					}}
				>
					<ListRow
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
				</EffectiveContextMenu>
			))}
		</div>
	);
}
