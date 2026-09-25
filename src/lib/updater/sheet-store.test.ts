import { beforeEach, describe, expect, it } from 'vitest';
import { useUpdateSheetStore } from './sheet-store';

describe('useUpdateSheetStore', () => {
	beforeEach(() => {
		useUpdateSheetStore.setState({ open: false, source: 'shell' });
	});

	it('starts closed on the shell tab', () => {
		const s = useUpdateSheetStore.getState();
		expect(s.open).toBe(false);
		expect(s.source).toBe('shell');
	});

	it('openSheet(source) opens on the requested tab — either banner can drive it', () => {
		useUpdateSheetStore.getState().openSheet('pkgs');
		expect(useUpdateSheetStore.getState()).toMatchObject({ open: true, source: 'pkgs' });

		useUpdateSheetStore.getState().openSheet('shell');
		expect(useUpdateSheetStore.getState()).toMatchObject({ open: true, source: 'shell' });
	});

	it('close() closes without changing which tab was last shown', () => {
		useUpdateSheetStore.getState().openSheet('pkgs');
		useUpdateSheetStore.getState().close();
		expect(useUpdateSheetStore.getState()).toMatchObject({ open: false, source: 'pkgs' });
	});
});
