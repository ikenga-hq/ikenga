// WP-72: the D-05 local tabs live at their own routes but read as People.

import { describe, expect, it } from 'vitest';

import { settingsIykeLine, settingsSection } from '@/shell/settings/nav';
import { searchSettings } from '@/shell/settings/search';

import { PEOPLE_TABS, tabScope } from './frame';

describe('People tabs', () => {
	it('are Profile, Devices, Members and Policies; Audit waits for WP-77', () => {
		expect(PEOPLE_TABS.map((t) => t.to)).toEqual([
			'/settings/profile',
			'/settings/devices',
			'/settings/members',
			'/settings/policies',
		]);
		expect(tabScope('members')).toBe('project');
		expect(tabScope('devices')).toBe('personal');
	});

	it('resolve to the People section for the nav highlight, header and iyke line', () => {
		expect(settingsSection('profile').id).toBe('people');
		expect(settingsSection('devices').id).toBe('people');
		expect(settingsSection('members').id).toBe('people');
		expect(settingsSection('policies').id).toBe('people');
		expect(settingsIykeLine(settingsSection('devices').id, 'personal')).toBe(
			'iyke settings open people'
		);
	});

	it('do not let prototype keys alias anything', () => {
		expect(settingsSection('toString').id).toBe('appearance');
	});

	it('make App lock findable from settings search', () => {
		const hits = searchSettings('app lock');
		expect(hits.some((h) => h.sectionId === 'people')).toBe(true);
	});
});
