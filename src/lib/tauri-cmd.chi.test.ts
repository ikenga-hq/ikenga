// Arg-shape guard for the Chi run commands. Tauri 2 deserialises each Rust
// command parameter by name, so the TS wrapper must match the signature in
// src-tauri/src/commands/chi.rs:
//   chi_run(.., opts: ChiRunOpts)              → invoke('chi_run', { opts })
//   chi_resume(.., runId: String, prompt: String) → invoke('chi_resume', { runId, prompt })
// A flat `invoke('chi_run', opts)` fails live with "missing required key opts".
import { beforeEach, describe, expect, it, vi } from 'vitest';

const invoke = vi.fn(async () => ({ run_id: 'run-1', status: 'queued' }));

vi.mock('./transport', () => ({
	getTransport: () => ({ invoke, listen: vi.fn() }),
	isRemoteWebSession: () => false,
	isTauri: () => false,
}));

import { chiResume, chiRun } from './tauri-cmd';

describe('chi command arg shapes', () => {
	beforeEach(() => invoke.mockClear());

	it('chiRun nests its options under `opts` (Rust `opts: ChiRunOpts`)', async () => {
		await chiRun({
			engineId: 'claude-code',
			prompt: 'summarise the diff',
			cwd: '/work/royalti-co',
			persistent: false,
		});
		expect(invoke).toHaveBeenCalledTimes(1);
		expect(invoke).toHaveBeenCalledWith('chi_run', {
			opts: {
				engineId: 'claude-code',
				prompt: 'summarise the diff',
				cwd: '/work/royalti-co',
				persistent: false,
			},
		});
		const args = (invoke.mock.calls[0] as unknown[])[1] as Record<string, unknown>;
		expect(Object.keys(args)).toEqual(['opts']);
	});

	it('chiResume sends `runId` and `prompt` as top-level args (Rust params)', async () => {
		await chiResume('run-9', 'keep going');
		expect(invoke).toHaveBeenCalledWith('chi_resume', { runId: 'run-9', prompt: 'keep going' });
	});
});
