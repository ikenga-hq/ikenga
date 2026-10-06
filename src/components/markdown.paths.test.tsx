// Audit 2026-10-06 rank 17 (UI): the markdown path linkifier raised an
// unhandled rejection when `fs_exists` rejected (the daemon rejects paths
// outside its allowlist), and in a browser it resolved relative tokens
// against the daemon's process cwd.

import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const env = vi.hoisted(() => ({
	remote: false,
	fsExists: vi.fn(async (_path: string): Promise<boolean> => {
		throw new Error('path outside allowlist');
	}),
}));

vi.mock('@/lib/transport', async (importOriginal) => ({
	...(await importOriginal<typeof import('@/lib/transport')>()),
	isRemoteWebSession: () => env.remote,
}));

vi.mock('@/lib/tauri-cmd', async (importOriginal) => ({
	...(await importOriginal<typeof import('@/lib/tauri-cmd')>()),
	fsExists: env.fsExists,
}));

vi.mock('@/lib/home', () => ({
	loadHome: async () => '/home/alice',
	getHomeSync: () => '/home/alice',
	shortPath: (p: string) => p,
}));

import { Markdown } from './markdown';

const unhandled: unknown[] = [];
const onUnhandled = (reason: unknown) => {
	unhandled.push(reason);
};

beforeEach(() => {
	env.fsExists.mockClear();
	unhandled.length = 0;
	process.on('unhandledRejection', onUnhandled);
});

afterEach(() => {
	cleanup();
	process.off('unhandledRejection', onUnhandled);
	env.remote = false;
});

function pills(): string[] {
	return screen
		.queryAllByRole('button')
		.map((b) => b.getAttribute('title') ?? '')
		.filter((t) => t.startsWith('Open '));
}

/** Let the pills' resolve effects (and any stray rejection) settle. */
async function settle() {
	await new Promise((r) => setTimeout(r, 20));
}

describe('markdown path pills on the desktop', () => {
	it('survives fs_exists rejecting, with no unhandled rejection', async () => {
		render(<Markdown content={'Open `docs/plan.md` and `/abs/notes.md`.'} />);
		await waitFor(() => expect(env.fsExists).toHaveBeenCalled());
		await settle();
		expect(pills()).toEqual(['Open docs/plan.md in viewer', 'Open /abs/notes.md in viewer']);
		expect(unhandled).toEqual([]);
	});
});

describe('markdown path pills in a browser session', () => {
	it('keeps a relative token as plain code instead of resolving it on the daemon', async () => {
		env.remote = true;
		render(<Markdown content={'See `docs/plan.md`, `/abs/notes.md` and [x](rel/y.md).'} />);
		await settle();
		expect(pills()).toEqual(['Open /abs/notes.md in viewer']);
		expect(screen.getByText('docs/plan.md').tagName).toBe('CODE');
		expect(screen.getByText('x').tagName).toBe('SPAN');
		expect(env.fsExists).not.toHaveBeenCalled();
		expect(unhandled).toEqual([]);
	});

	it('links a relative token once a cwd anchors it', async () => {
		env.remote = true;
		render(<Markdown cwd="/srv/project" content={'See `docs/plan.md`.'} />);
		await settle();
		expect(pills()).toEqual(['Open /srv/project/docs/plan.md in viewer']);
		expect(env.fsExists).not.toHaveBeenCalled();
	});
});
