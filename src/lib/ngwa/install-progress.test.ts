import { describe, expect, it } from 'vitest';
import { classifyInstallError } from './install-errors';
import {
	installProgressReducer,
	type InstallProgressEvent,
	type InstallProgressState,
} from './install-progress';

function ev(p: Partial<InstallProgressEvent> & Pick<InstallProgressEvent, 'stage'>): InstallProgressEvent {
	return {
		installId: 'row-1',
		pkgId: 'com.ikenga.meetings',
		label: '',
		cancellable: true,
		...p,
	};
}

function run(actions: Parameters<typeof installProgressReducer>[1][]): InstallProgressState {
	return actions.reduce(installProgressReducer, {} as InstallProgressState);
}

const start = { type: 'start', key: 'row-1', name: 'Meetings', verb: 'install' } as const;

describe('installProgressReducer', () => {
	it('starts resolving, or queued for Update all', () => {
		expect(run([start])['row-1']).toMatchObject({ stage: 'resolving', status: 'running', percent: null });
		expect(run([{ ...start, queued: true }])['row-1']).toMatchObject({ stage: 'queued', label: 'Queued' });
	});

	it('follows the installer stages with a determinate download', () => {
		const s = run([
			start,
			{ type: 'step', key: 'row-1', index: 0, total: 2, pkgId: 'com.dep' },
			{ type: 'event', event: ev({ stage: 'downloading', label: 'Downloading', percent: 42.5, bytes: 10, total: 20 }) },
		]);
		expect(s['row-1']).toMatchObject({
			stage: 'downloading',
			label: 'Downloading',
			percent: 42.5,
			step: { index: 0, total: 2, pkgId: 'com.dep' },
			cancellable: true,
		});
	});

	it('is indeterminate without a percent and keeps the npm detail within its stage', () => {
		let s = run([start, { type: 'event', event: ev({ stage: 'installing_deps', detail: '3 packages fetched' }) }]);
		expect(s['row-1']).toMatchObject({ percent: null, detail: '3 packages fetched', label: 'Installing dependencies' });
		s = installProgressReducer(s, { type: 'event', event: ev({ stage: 'installing_deps' }) });
		expect(s['row-1'].detail).toBe('3 packages fetched');
		s = installProgressReducer(s, { type: 'event', event: ev({ stage: 'registering', cancellable: false }) });
		expect(s['row-1']).toMatchObject({ detail: null, cancellable: false });
	});

	it('never lets cancel through once registering, even if the event says otherwise', () => {
		const s = run([
			start,
			{ type: 'event', event: ev({ stage: 'registering', cancellable: true }) },
			{ type: 'cancel-requested', key: 'row-1' },
		]);
		expect(s['row-1']).toMatchObject({ cancellable: false, cancelRequested: false });
	});

	it('records a cancel request while cancellable', () => {
		const s = run([start, { type: 'cancel-requested', key: 'row-1' }]);
		expect(s['row-1'].cancelRequested).toBe(true);
	});

	it('settles to failed with the classified error, or cancelled', () => {
		const failed = run([
			start,
			{ type: 'failed', key: 'row-1', error: classifyInstallError('ENOSPC: no space left on device', 'Meetings') },
		]);
		expect(failed['row-1']).toMatchObject({ status: 'failed', cancellable: false });
		expect(failed['row-1'].error?.kind).toBe('disk-space');

		const cancelled = run([
			start,
			{ type: 'failed', key: 'row-1', error: classifyInstallError('install cancelled', 'Meetings') },
		]);
		expect(cancelled['row-1'].status).toBe('cancelled');
	});

	it('drops events for settled or unknown runs', () => {
		const s = run([start, { type: 'succeeded', key: 'row-1' }]);
		const after = installProgressReducer(s, { type: 'event', event: ev({ stage: 'downloading', percent: 5 }) });
		expect(after).toBe(s);
		expect(installProgressReducer({}, { type: 'event', event: ev({ stage: 'downloading' }) })).toEqual({});
	});

	it('keeps concurrent runs apart', () => {
		const s = run([
			start,
			{ type: 'start', key: 'row-2', name: 'Notes', verb: 'update' },
			{ type: 'event', event: ev({ stage: 'extracting' }) },
			{ type: 'event', event: ev({ installId: 'row-2', stage: 'downloading', percent: 80 }) },
		]);
		expect(s['row-1'].stage).toBe('extracting');
		expect(s['row-2']).toMatchObject({ stage: 'downloading', percent: 80 });
	});

	it('clears a run (Retry starts fresh)', () => {
		const s = run([start, { type: 'clear', key: 'row-1' }]);
		expect(s['row-1']).toBeUndefined();
	});
});
