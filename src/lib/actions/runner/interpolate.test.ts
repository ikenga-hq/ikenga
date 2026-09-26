import { describe, expect, it } from 'vitest';

import {
	basename,
	emptyRunVariables,
	interpolate,
	mentionsVariable,
	templateVariables,
	UnknownVariableError,
	type RunVariables,
} from './interpolate';

function vars(partial: Partial<RunVariables>): RunVariables {
	return { ...emptyRunVariables(), ...partial };
}

describe('interpolate (G-ACTIONS §8.2)', () => {
	it('scans names like the Rust validator, spaces included', () => {
		expect(templateVariables('a {{file.path}} b {{ branch }} {{x')).toEqual(['file.path', ' branch ']);
		expect(() => interpolate('{{ file.path }}', vars({}), 'raw')).toThrow(UnknownVariableError);
		expect(() => interpolate('{{nope}}', vars({}), 'raw')).toThrow(/not a run variable/);
	});

	it('knows all six variables, file.name included (DEC-63.4)', () => {
		const v = vars({
			'file.path': '/p/a.ts',
			'file.name': 'a.ts',
			selection: 'sel',
			'project.root': '/p',
			'pane.url': '/tasks',
			branch: 'main',
		});
		expect(
			interpolate('{{file.path}}|{{file.name}}|{{selection}}|{{project.root}}|{{pane.url}}|{{branch}}', v, 'raw')
		).toBe('/p/a.ts|a.ts|sel|/p|/tasks|main');
	});

	it('a missing value interpolates as empty', () => {
		expect(interpolate('x{{branch}}y', {}, 'raw')).toBe('xy');
		expect(interpolate('echo {{branch}}', {}, 'uri')).toBe('echo ');
	});

	it('has no shell modes: shell values travel as env vars in Rust, never as text', () => {
		// Display-only raw substitution is all the frontend does for `shell`.
		const hostile = `a'; rm -rf ~ #$(whoami)`;
		expect(interpolate('cat {{file.path}}', vars({ 'file.path': hostile }), 'raw')).toBe(`cat ${hostile}`);
	});

	it('uri: percent-encodes values, except a url that is one variable', () => {
		expect(interpolate('/search?q={{selection}}', vars({ selection: 'a b&c' }), 'uri')).toBe('/search?q=a%20b%26c');
		expect(interpolate('{{selection}}', vars({ selection: 'https://x.dev/a b' }), 'uri')).toBe('https://x.dev/a b');
	});

	it('helpers', () => {
		expect(basename('/a/b/c.ts')).toBe('c.ts');
		expect(basename('C:\\a\\b.rs')).toBe('b.rs');
		expect(basename('/a/dir/')).toBe('dir');
		expect(mentionsVariable('branch', 'x', undefined, 'on {{branch}}')).toBe(true);
		expect(mentionsVariable('branch', 'x')).toBe(false);
	});
});
