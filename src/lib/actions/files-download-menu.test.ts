import { describe, expect, it } from 'vitest';
import { DEFAULT_MENUS } from './menus';

describe('files.download placement', () => {
	it('is in the Files row menu for file rows only', () => {
		const entry = DEFAULT_MENUS.files.find(
			(e) => typeof e !== 'string' && e.id === 'files.download'
		);
		expect(entry).toMatchObject({ id: 'files.download', condition: 'file' });
	});

	it('is in the viewer pane menu for artifact tabs', () => {
		const entry = DEFAULT_MENUS.pane.find(
			(e) => typeof e !== 'string' && e.id === 'files.download'
		);
		expect(entry).toMatchObject({ id: 'files.download', condition: 'artifact-tab' });
	});
});
