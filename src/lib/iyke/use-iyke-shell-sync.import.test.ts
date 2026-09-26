// WP-62 review (S4): `POST /iyke/actions/import`'s add/skip/overwrite
// behaviour, isolated from `use-iyke-shell-sync.test.ts` because it needs
// `saveUserAction` and `getEffectiveModel` mocked — the parent file's tests
// rely on the real `./keymap-bridge` (EMPTY_MODEL, real merge) instead.

import { beforeEach, describe, expect, it, vi } from 'vitest';

const saveUserAction = vi.fn((_scope: unknown, _action: unknown) => Promise.resolve());
const getEffectiveModel = vi.fn();

vi.mock('./keymap-bridge', async () => {
	const actual = await vi.importActual<typeof import('./keymap-bridge')>('./keymap-bridge');
	return {
		...actual,
		saveUserAction: (scope: unknown, action: unknown) => saveUserAction(scope, action),
		getEffectiveModel: () => getEffectiveModel(),
	};
});

const iykeActionsRequestDone = vi.fn((_requestId: string, _result: unknown) => Promise.resolve());
vi.mock('@/lib/tauri-cmd', () => ({
	iykeActionsRequestDone: (requestId: string, result: unknown) => iykeActionsRequestDone(requestId, result),
}));

import {
	existingActionIds,
	handleActionsImportRequest,
	type ActionsImportRequestPayload,
} from './use-iyke-shell-sync';
import type { EffectiveAction, EffectiveModel } from './keymap-bridge';

function fakeAction(overrides: Partial<EffectiveAction> = {}): EffectiveAction {
	return {
		id: 'a',
		name: 'A',
		description: '',
		source: 'personal',
		run: { kind: 'chi', target: 'active', prompt: 'x' },
		placements: [],
		locked: false,
		danger: false,
		hosted: false,
		osOnly: false,
		editable: true,
		...overrides,
	} as EffectiveAction;
}

function fakeModel(overrides: Partial<EffectiveModel> = {}): EffectiveModel {
	return {
		actions: [],
		shadowedActions: [],
		projectId: null,
		...overrides,
	} as unknown as EffectiveModel;
}

describe('existingActionIds (S4)', () => {
	it('collects ids in force for the scope, plus personal ids the project shadows', () => {
		const model = fakeModel({
			actions: [
				fakeAction({ id: 'p1', source: 'personal' }),
				fakeAction({ id: 'j1', source: 'project' }),
			],
			shadowedActions: [fakeAction({ id: 'shadowed', source: 'personal', overriddenBy: 'project' })],
		});

		expect(existingActionIds('personal', model)).toEqual(new Set(['p1', 'shadowed']));
		expect(existingActionIds('project', model)).toEqual(new Set(['j1']));
	});
});

describe('handleActionsImportRequest (S4)', () => {
	beforeEach(() => {
		saveUserAction.mockClear();
		saveUserAction.mockImplementation(() => Promise.resolve());
		getEffectiveModel.mockReset();
		iykeActionsRequestDone.mockClear();
	});

	function payload(overrides: Partial<ActionsImportRequestPayload> = {}): ActionsImportRequestPayload {
		return {
			request_id: 'req-1',
			scope: 'personal',
			actions: [],
			...overrides,
		} as ActionsImportRequestPayload;
	}

	it('skips an id already present in the scope file, and does not write it', async () => {
		getEffectiveModel.mockReturnValue(
			fakeModel({ actions: [fakeAction({ id: 'explain-file', source: 'personal' })] })
		);

		await handleActionsImportRequest(
			payload({ actions: [{ id: 'explain-file', name: 'X', run: { kind: 'skill', skill: 'x' } } as never] })
		);

		expect(saveUserAction).not.toHaveBeenCalled();
		const [, result] = iykeActionsRequestDone.mock.calls[0];
		expect(result).toEqual({ ok: true, added: [], skipped: ['explain-file'], errors: [] });
	});

	it('writes over an existing id when overwrite: true', async () => {
		getEffectiveModel.mockReturnValue(
			fakeModel({ actions: [fakeAction({ id: 'explain-file', source: 'personal' })] })
		);

		await handleActionsImportRequest(
			payload({
				overwrite: true,
				actions: [{ id: 'explain-file', name: 'X', run: { kind: 'skill', skill: 'x' } } as never],
			})
		);

		expect(saveUserAction).toHaveBeenCalledTimes(1);
		const [, result] = iykeActionsRequestDone.mock.calls[0];
		expect(result).toEqual({ ok: true, added: ['explain-file'], skipped: [], errors: [] });
	});

	it('adds a new id and reports a per-item error without sinking the rest of the batch', async () => {
		getEffectiveModel.mockReturnValue(fakeModel());
		saveUserAction.mockImplementationOnce(() => Promise.resolve());
		saveUserAction.mockImplementationOnce(() => Promise.reject(new Error('E_RUN_KIND: bad run')));

		await handleActionsImportRequest(
			payload({
				actions: [
					{ id: 'good', name: 'Good', run: { kind: 'skill', skill: 'x' } } as never,
					{ id: 'bad', name: 'Bad', run: { kind: 'skill', skill: 'x' } } as never,
				],
			})
		);

		expect(saveUserAction).toHaveBeenCalledTimes(2);
		const [, result] = iykeActionsRequestDone.mock.calls[0];
		expect(result).toEqual({
			ok: true,
			added: ['good'],
			skipped: [],
			errors: [{ id: 'bad', error: 'E_RUN_KIND: bad run' }],
		});
	});
});
