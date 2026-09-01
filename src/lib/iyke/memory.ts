// Thin client for the iyke memory endpoints (Phase 1 of
// projects-first-class). Same bearer-token contract as the MCP server —
// goes through iykeFetch so the FE shares one auth + transport path
// with every other caller.

import { listen, type UnlistenFn } from '@/lib/transport';

import { iykeFetch } from './client';

async function postJson<T>(path: string, body: unknown): Promise<T> {
	const res = await iykeFetch(path, {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(body),
	});
	if (!res.ok) {
		throw new Error(`iyke ${path} ${res.status}: ${await res.text()}`);
	}
	return (await res.json()) as T;
}

async function getJson<T>(path: string, params?: Record<string, unknown>): Promise<T> {
	const qs = new URLSearchParams();
	if (params) {
		for (const [k, v] of Object.entries(params)) {
			if (v === undefined || v === null || v === '') continue;
			qs.set(k, String(v));
		}
	}
	const q = qs.toString();
	const res = await iykeFetch(`${path}${q ? `?${q}` : ''}`);
	if (!res.ok) {
		if (res.status === 404) {
			return null as unknown as T;
		}
		throw new Error(`iyke ${path} ${res.status}: ${await res.text()}`);
	}
	return (await res.json()) as T;
}

// ── Scratchpads ──────────────────────────────────────────────────────

export interface ScratchpadListEntry {
	id: string;
	name: string;
	updated_at: number;
	preview: string;
}
export interface ScratchpadListResponse {
	scope: string;
	scratchpads: ScratchpadListEntry[];
}
export interface ScratchpadChangedEvent {
	scope: string;
	name: string;
	action: 'write' | 'append' | 'delete';
	updated_at?: number;
}

export interface ScratchpadReadResponse {
	id: string;
	scope: string;
	name: string;
	body: string;
	updated_at: number;
}

export function listenScratchpadChanges(
	callback: (payload: ScratchpadChangedEvent) => void
): Promise<UnlistenFn> {
	return listen<ScratchpadChangedEvent>('iyke://scratchpad-changed', (event) =>
		callback(event.payload)
	);
}

export function listScratchpads(scope?: string) {
	return getJson<ScratchpadListResponse>('/iyke/scratchpad/list', { scope });
}
export function readScratchpad(name: string, scope?: string) {
	return getJson<ScratchpadReadResponse | null>('/iyke/scratchpad/read', { scope, name });
}
export function writeScratchpad(name: string, body: string, scope?: string) {
	return postJson<{ id: string; scope: string; updated_at: number }>('/iyke/scratchpad/write', {
		scope: scope ?? null,
		name,
		body,
	});
}
export function deleteScratchpad(name: string, scope?: string) {
	return postJson<{ ok: boolean }>('/iyke/scratchpad/delete', { scope: scope ?? null, name });
}

// ── Todos ────────────────────────────────────────────────────────────

export type TodoStatus = 'open' | 'in_progress' | 'blocked' | 'done' | 'cancelled';

export interface Todo {
	id: string;
	scope: string;
	title: string;
	body: string | null;
	status: TodoStatus;
	tags: string[];
	blocker_id: string | null;
	assignee: string | null;
	created_at: number;
	updated_at: number;
	completed_at: number | null;
}
export interface TodoListResponse {
	scope: string;
	todos: Todo[];
}

export function listTodos(opts?: {
	scope?: string;
	status?: TodoStatus;
	tag?: string;
	assignee?: string;
}) {
	return getJson<TodoListResponse>('/iyke/todo/list', opts);
}

export function createTodo(args: {
	scope?: string;
	title: string;
	body?: string;
	tags?: string[];
	assignee?: string;
	blocker_id?: string;
}) {
	return postJson<{ id: string; scope: string; created_at: number }>('/iyke/todo/create', {
		scope: args.scope ?? null,
		title: args.title,
		body: args.body ?? null,
		tags: args.tags ?? [],
		assignee: args.assignee ?? null,
		blocker_id: args.blocker_id ?? null,
	});
}

export function updateTodo(args: {
	id: string;
	status?: TodoStatus;
	title?: string;
	body?: string;
	assignee?: string;
	blocker_id?: string;
}) {
	return postJson<{ id: string; updated_at: number }>('/iyke/todo/update', args);
}

export function completeTodo(id: string) {
	return postJson<{ id: string; completed_at: number }>('/iyke/todo/complete', { id });
}

// ── Timers ───────────────────────────────────────────────────────────

export type TimerStatus = 'pending' | 'fired' | 'cancelled';

export interface Timer {
	id: string;
	scope: string;
	fire_at: number;
	agent_id: string | null;
	title: string;
	body: string | null;
	status: TimerStatus;
	created_at: number;
	fired_at: number | null;
}

export function listTimers(opts?: { scope?: string; status?: TimerStatus }) {
	return getJson<{ scope: string; timers: Timer[] }>('/iyke/timer/list', opts);
}

export function scheduleTimer(args: {
	scope?: string;
	title: string;
	body?: string;
	agent_id?: string;
	fire_at?: number;
	delay_ms?: number;
}) {
	return postJson<{ id: string; scope: string; fire_at: number; agent_id: string | null }>(
		'/iyke/timer/schedule',
		{
			scope: args.scope ?? null,
			title: args.title,
			body: args.body ?? null,
			agent_id: args.agent_id ?? null,
			fire_at: args.fire_at ?? null,
			delay_ms: args.delay_ms ?? null,
		}
	);
}

export function cancelTimer(id: string) {
	return postJson<{ cancelled: boolean }>('/iyke/timer/cancel', { id });
}
