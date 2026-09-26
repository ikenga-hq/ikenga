// WP-58 — Editor form-model unit tests (written under DEC-50, not run by
// WP-58; WP-63 runs them). G-ACTIONS §1.2/§1.3 (action + placement shape),
// §4 (`when`), §5 (the DEC-59 conflict rule) and §8.1/§8.2 (run kinds, the
// six variables).

import { describe, expect, it } from 'vitest';
import type { EffectiveKeymapEntry } from '@/lib/actions/store';
import {
	buildKeybindingRule,
	buildPlacements,
	buildRun,
	buildUserAction,
	derivedKeyWhen,
	effectiveKeyWhen,
	emptyForm,
	findEditorConflict,
	missingRunField,
	slug,
	suggestedRestriction,
	type EditorFormState,
} from './form-model';

function withForm(patch: Partial<EditorFormState>): EditorFormState {
	return { ...emptyForm(), ...patch };
}

describe('slug', () => {
	it('lowercases, hyphenates, and strips leading/trailing hyphens', () => {
		expect(slug('Explain This File!')).toBe('explain-this-file');
		expect(slug('  --Refresh Pulse--  ')).toBe('refresh-pulse');
	});

	it('falls back to "untitled" when nothing survives', () => {
		expect(slug('   ')).toBe('untitled');
		expect(slug('!!!')).toBe('untitled');
	});
});

describe('buildRun', () => {
	it('builds a chi run, omitting engineId unless the target is "engine"', () => {
		const form = withForm({ runType: 'chi', chiTarget: 'active', chiPrompt: 'hi', chiEngineId: 'gemini' });
		expect(buildRun(form)).toEqual({ kind: 'chi', target: 'active', prompt: 'hi' });
	});

	it('includes engineId only when target is "engine"', () => {
		const form = withForm({ runType: 'chi', chiTarget: 'engine', chiPrompt: 'hi', chiEngineId: 'gemini' });
		expect(buildRun(form)).toEqual({ kind: 'chi', target: 'engine', prompt: 'hi', engineId: 'gemini' });
	});

	it('omits cwd and confirm when unset (shell)', () => {
		const form = withForm({ runType: 'shell', shellCommand: 'echo hi' });
		expect(buildRun(form)).toEqual({ kind: 'shell', command: 'echo hi' });
	});

	it('includes cwd and confirm when set (shell)', () => {
		const form = withForm({
			runType: 'shell',
			shellCommand: 'echo hi',
			shellCwd: '{{project.root}}',
			shellConfirm: true,
		});
		expect(buildRun(form)).toEqual({
			kind: 'shell',
			command: 'echo hi',
			cwd: '{{project.root}}',
			confirm: true,
		});
	});

	it('omits method for the iyke default (POST), includes it for GET', () => {
		const post = withForm({ runType: 'iyke', iykeRoute: '/pane/navigate', iykeMethod: 'POST' });
		expect(buildRun(post)).toEqual({ kind: 'iyke', route: '/pane/navigate' });
		const get = withForm({ runType: 'iyke', iykeRoute: '/pane/navigate', iykeMethod: 'GET' });
		expect(buildRun(get)).toEqual({ kind: 'iyke', route: '/pane/navigate', method: 'GET' });
	});

	it('builds skill, workflow and open runs', () => {
		expect(buildRun(withForm({ runType: 'skill', skillName: 'release-status' }))).toEqual({
			kind: 'skill',
			skill: 'release-status',
		});
		expect(buildRun(withForm({ runType: 'workflow', workflowName: 'nightly-pulse' }))).toEqual({
			kind: 'workflow',
			workflow: 'nightly-pulse',
		});
		expect(buildRun(withForm({ runType: 'open', openUrl: '/ngwa/installed' }))).toEqual({
			kind: 'open',
			url: '/ngwa/installed',
		});
	});
});

describe('buildPlacements (§1.3 — the full menu id, never D-06\'s bare "section")', () => {
	it('is empty when nothing is checked', () => {
		expect(buildPlacements(emptyForm())).toEqual([]);
	});

	it('carries the files glob as a placement `when`, not a bare `section` field', () => {
		const form = withForm({ placements: { ...emptyForm().placements, files: true }, filesGlob: '*.{ts,rs}' });
		expect(buildPlacements(form)).toEqual([{ at: 'files', when: "resource =~ '*.{ts,rs}'" }]);
	});

	it('has no `when` for files with an empty glob', () => {
		const form = withForm({ placements: { ...emptyForm().placements, files: true }, filesGlob: '' });
		expect(buildPlacements(form)).toEqual([{ at: 'files' }]);
	});

	it('parameterizes section and native onto the full menu id', () => {
		const form = withForm({
			placements: { ...emptyForm().placements, section: true, native: true },
			sectionId: 'ngwa-project',
			nativeTop: 'chi',
		});
		expect(buildPlacements(form)).toEqual([{ at: 'section/ngwa-project' }, { at: 'native/chi' }]);
	});
});

describe('buildUserAction (§1.2)', () => {
	it('omits icon when it is the default "zap", and description when blank', () => {
		const action = buildUserAction(withForm({ name: 'x', id: 'x', runType: 'open', openUrl: '/x' }), 'personal');
		expect(action).not.toHaveProperty('icon');
		expect(action).not.toHaveProperty('description');
		expect(action.scope).toBe('personal');
	});

	it('never carries a `key` field (§1.2: keys live only in keybindings.json)', () => {
		const action = buildUserAction(
			withForm({ name: 'x', id: 'x', runType: 'open', openUrl: '/x', key: 'mod+shift+e' }),
			'personal'
		);
		expect(action).not.toHaveProperty('key');
	});
});

describe('derivedKeyWhen / effectiveKeyWhen', () => {
	it('is "always" with no Files placement', () => {
		expect(derivedKeyWhen(emptyForm())).toBe('always');
	});

	it('derives filesFocus + the glob once Files is checked', () => {
		const form = withForm({ placements: { ...emptyForm().placements, files: true }, filesGlob: '*.ts' });
		expect(derivedKeyWhen(form)).toBe("filesFocus && resource =~ '*.ts'");
	});

	it('effectiveKeyWhen follows the derived value until the When field is touched', () => {
		const form = withForm({ placements: { ...emptyForm().placements, files: true }, filesGlob: '*.ts' });
		expect(effectiveKeyWhen(form)).toBe(derivedKeyWhen(form));
		const touched = { ...form, whenTouched: true, whenValue: 'paneFocus' };
		expect(effectiveKeyWhen(touched)).toBe('paneFocus');
	});
});

describe('buildKeybindingRule', () => {
	it('is null with no key', () => {
		expect(buildKeybindingRule(emptyForm())).toBeNull();
	});

	it('omits `when` for the "always" default', () => {
		const form = withForm({ key: 'mod+shift+e' });
		expect(buildKeybindingRule(form)).toEqual({ key: 'mod+shift+e', command: '' });
	});

	it('carries a narrower `when`', () => {
		const form = withForm({ key: 'mod+shift+e', id: 'explain-file', whenTouched: true, whenValue: 'filesFocus' });
		expect(buildKeybindingRule(form)).toEqual({ key: 'mod+shift+e', command: 'explain-file', when: 'filesFocus' });
	});
});

describe('missingRunField', () => {
	it('flags an empty required field per run kind', () => {
		expect(missingRunField(withForm({ runType: 'chi', chiPrompt: '' }))).toBe(true);
		expect(missingRunField(withForm({ runType: 'chi', chiPrompt: 'hi' }))).toBe(false);
		expect(missingRunField(withForm({ runType: 'shell', shellCommand: '  ' }))).toBe(true);
		expect(missingRunField(withForm({ runType: 'open', openUrl: '/x' }))).toBe(false);
	});
});

describe('findEditorConflict (DEC-59, §5 — never string equality)', () => {
	function entry(over: Partial<EffectiveKeymapEntry>): EffectiveKeymapEntry {
		return { command: 'other.command', key: 'mod+shift+e', when: 'always', source: 'default', label: '', ...over };
	}

	it('is null with no key set', () => {
		expect(findEditorConflict([], 'explain-file', emptyForm(), 'mac')).toBeNull();
	});

	it('flags a clash: same platform key, same normalized `when`', () => {
		const entries = [entry({})];
		const form = withForm({ key: 'mod+shift+e' }); // whenValue defaults to 'always' untouched
		const conflict = findEditorConflict(entries, 'explain-file', form, 'mac');
		expect(conflict?.other.command).toBe('other.command');
	});

	it('is not a clash when the normalized `when` differs (precedence, not a clash)', () => {
		const entries = [entry({ when: 'terminalFocus' })];
		const form = withForm({ key: 'mod+shift+e' }); // 'always' vs 'terminalFocus'
		expect(findEditorConflict(entries, 'explain-file', form, 'mac')).toBeNull();
	});

	it('ignores the action\'s own prior binding', () => {
		const entries = [entry({ command: 'explain-file' })];
		const form = withForm({ key: 'mod+shift+e' });
		expect(findEditorConflict(entries, 'explain-file', form, 'mac')).toBeNull();
	});

	it('never matches by raw string equality of `when` alone — normalizes first', () => {
		// "always" (the omitted/default form) vs "true" (DEC-62 §4.1 literal for
		// the same normalized meaning) must still clash, though the two are not
		// string-equal.
		const entries = [entry({ when: 'true' })];
		const form = withForm({ key: 'mod+shift+e' });
		const conflict = findEditorConflict(entries, 'explain-file', form, 'mac');
		expect(conflict?.other.command).toBe('other.command');
	});
});

describe('suggestedRestriction', () => {
	it('suggests filesFocus + the glob when Files is checked', () => {
		const form = withForm({ placements: { ...emptyForm().placements, files: true }, filesGlob: '*.rs' });
		expect(suggestedRestriction(form)).toBe("filesFocus && resource =~ '*.rs'");
	});

	it('falls back to !inputFocus otherwise', () => {
		expect(suggestedRestriction(emptyForm())).toBe('!inputFocus');
	});
});
