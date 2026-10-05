// WP-72: the D-05 local tabs live at their own routes but read as People.

import { describe, expect, it } from 'vitest';

import { settingsIykeLine, settingsSection } from '@/shell/settings/nav';
import { searchSettings } from '@/shell/settings/search';

import { accessStorePath, PEOPLE_TABS, TAB_SCOPE_WHY, TAB_SCOPES, tabScope } from './frame';

describe('People tabs', () => {
	it('are Profile, Devices, Members, Policies and Audit (WP-77)', () => {
		expect(PEOPLE_TABS.map((t) => t.to)).toEqual([
			'/settings/profile',
			'/settings/devices',
			'/settings/members',
			'/settings/policies',
			'/settings/audit',
		]);
		// D-05 `TABS[].scopes`: Audit is live at both scopes (§11.1).
		expect(TAB_SCOPES.audit).toEqual(['personal', 'project']);
		expect(tabScope('audit')).toBe('personal');
		expect(TAB_SCOPE_WHY.audit).toBeUndefined();
		expect(tabScope('members')).toBe('project');
		expect(tabScope('devices')).toBe('personal');
		// D-05 `#scopeSw`: the other scope is disabled with the tab's reason.
		expect(TAB_SCOPE_WHY.members).toBe('People are invited to a project, not to a machine.');
		expect(TAB_SCOPE_WHY.policies).toBe('Roles are defined per project.');
	});

	it('name the access store in the file bar (G-ACCESS §11.2 D-4)', () => {
		expect(accessStorePath(true)).toBe('server operator database');
		expect(accessStorePath(false)).toBe('<data-dir>/access.db');
	});

	it('resolve to the People section for the nav highlight, header and iyke line', () => {
		expect(settingsSection('profile').id).toBe('people');
		expect(settingsSection('devices').id).toBe('people');
		expect(settingsSection('members').id).toBe('people');
		expect(settingsSection('policies').id).toBe('people');
		expect(settingsSection('audit').id).toBe('people');
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

	it('make the audit log findable from settings search', () => {
		const hits = searchSettings('audit');
		expect(hits.some((h) => h.sectionId === 'people')).toBe(true);
	});
});
