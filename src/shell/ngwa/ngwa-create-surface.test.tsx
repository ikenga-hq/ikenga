import { render, screen, fireEvent, cleanup, waitFor } from '@testing-library/react';
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { NgwaCreateSurface } from './ngwa-create-surface';
import * as tauriCmd from '@/lib/tauri-cmd';
import { useCompanionStore } from '@/shell/companion/companion-store';

vi.mock('@/lib/tauri-cmd', () => ({
	pkgScaffold: vi.fn(),
}));

describe('NgwaCreateSurface — WP-24 / locked D-02', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		useCompanionStore.getState().setDraft('');
		useCompanionStore.getState().setState('collapsed');
	});

	afterEach(() => {
		cleanup();
	});

	it('renders all 12 equipment kinds with Skill selected by default', () => {
		render(<NgwaCreateSurface />);

		// Verify 12 kinds are in the sidebar
		const kinds = [
			'artifact',
			'app',
			'tool',
			'engine',
			'sidecar',
			'skill',
			'agent',
			'command',
			'hook',
			'workflow',
			'schedule',
			'project',
		];

		for (const kind of kinds) {
			expect(screen.getByTestId(`kind-select-${kind}`)).toBeDefined();
		}

		// Skill is selected by default
		const skillBtn = screen.getByTestId('kind-select-skill');
		expect(skillBtn.getAttribute('aria-selected')).toBe('true');

		// Check head file preview title in tri-pane preview
		expect(screen.getAllByText('SKILL.md').length).toBeGreaterThan(0);
		expect(screen.getByText('_templates/skill')).toBeDefined();
	});

	it('switches kind and updates questions, chips, and previews', () => {
		render(<NgwaCreateSurface />);

		// Switch to Agent
		const agentBtn = screen.getByTestId('kind-select-agent');
		fireEvent.click(agentBtn);

		expect(agentBtn.getAttribute('aria-selected')).toBe('true');
		expect(screen.getByText('What is its role and persona?')).toBeDefined();
		expect(screen.getAllByText('agent.md').length).toBeGreaterThan(0);
		expect(screen.getByText('_blueprints/agent')).toBeDefined();
	});

	it('validates slug format and updates live manifest preview', () => {
		render(<NgwaCreateSurface />);

		const slugInput = screen.getByTestId('input-slug') as HTMLInputElement;

		// Initial valid slug
		expect(slugInput.value).toBe('release-notes');
		expect(screen.getByText('io.royalti.release-notes')).toBeDefined();

		// Change slug to invalid format (e.g. spaces or uppercase become lower-hyphenated)
		fireEvent.change(slugInput, { target: { value: 'my release notes' } });
		expect(slugInput.value).toBe('my-release-notes');
		expect(screen.getByText('io.royalti.my-release-notes')).toBeDefined();

		// Set to 1 character (invalid)
		fireEvent.change(slugInput, { target: { value: 'a' } });
		expect(
			screen.getByText(/Slug must contain lowercase letters, numbers, and hyphens/i)
		).toBeDefined();

		const submitBtn = screen.getByTestId('scaffold-submit-btn') as HTMLButtonElement;
		expect(submitBtn.disabled).toBe(true);
	});

	it('validates description length threshold (>= 20 characters)', () => {
		render(<NgwaCreateSurface />);

		const descInput = screen.getByTestId('input-description') as HTMLTextAreaElement;

		// Change description to < 20 chars
		fireEvent.change(descInput, { target: { value: 'Short desc' } });

		expect(screen.getByText(/minimum 20 characters required/i)).toBeDefined();

		const submitBtn = screen.getByTestId('scaffold-submit-btn') as HTMLButtonElement;
		expect(submitBtn.disabled).toBe(true);

		// Change description to >= 20 chars
		fireEvent.change(descInput, {
			target: { value: 'This is a sufficiently long description that satisfies the threshold.' },
		});
		expect(screen.getByText(/enough for an engine to match against/i)).toBeDefined();
		expect(submitBtn.disabled).toBe(false);
	});

	it('toggles scope between personal and project', () => {
		render(<NgwaCreateSurface />);

		const personalBtn = screen.getByTestId('scope-personal-btn');
		const projectBtn = screen.getByTestId('scope-project-btn');

		// Click personal
		fireEvent.click(personalBtn);
		expect(screen.getByText(/~\/\.claude\/skills\/release-notes\//i)).toBeDefined();

		// Click project
		fireEvent.click(projectBtn);
		expect(screen.getByText(/\.claude\/skills\/release-notes\//i)).toBeDefined();
	});

	it('toggles capability chips and reflects in head file preview', () => {
		render(<NgwaCreateSurface />);

		const bashChip = screen.getByTestId('chip-Bash');
		fireEvent.click(bashChip); // select Bash

		// Bash should now be in the allowed-tools in the preview pane
		const previewCol = screen.getByTestId('ngwa-create-preview-col');
		expect(previewCol.textContent).toContain('Bash');
	});

	it('calls pkgScaffold on submit and shows success banner', async () => {
		vi.mocked(tauriCmd.pkgScaffold).mockResolvedValueOnce({
			ok: true,
			kind: 'skill',
			slug: 'release-notes',
			targetPath: '/path/to/project/.claude/skills/release-notes/SKILL.md',
			targetFolder: '/path/to/project/.claude/skills/release-notes',
			filesWritten: ['SKILL.md', 'manifest.json', 'package.json'],
		});

		render(<NgwaCreateSurface />);

		const submitBtn = screen.getByTestId('scaffold-submit-btn') as HTMLButtonElement;
		expect(submitBtn.disabled).toBe(false);

		fireEvent.click(submitBtn);

		await waitFor(() => {
			expect(tauriCmd.pkgScaffold).toHaveBeenCalledWith(
				expect.objectContaining({
					kind: 'skill',
					slug: 'release-notes',
					authorName: 'Royalti',
				})
			);
			expect(screen.getByTestId('scaffold-success-banner')).toBeDefined();
		});
	});

	it('calls pkgScaffold and stages brief in Companion store when clicking Scaffold + brief a Chi (WP-26)', async () => {
		vi.mocked(tauriCmd.pkgScaffold).mockResolvedValueOnce({
			ok: true,
			kind: 'skill',
			slug: 'release-notes',
			targetPath: '/path/to/project/.claude/skills/release-notes/SKILL.md',
			targetFolder: '/path/to/project/.claude/skills/release-notes',
			filesWritten: ['SKILL.md', 'manifest.json', 'package.json'],
		});

		render(<NgwaCreateSurface />);

		const briefBtn = screen.getByTestId('scaffold-brief-btn');
		fireEvent.click(briefBtn);

		await waitFor(() => {
			expect(tauriCmd.pkgScaffold).toHaveBeenCalled();
		});

		// Verify Companion store has dispatch draft populated, state expanded, and focus pending
		const storeState = useCompanionStore.getState();
		expect(storeState.state).toBe('expanded');
		expect(storeState.focusPending).toBe(true);
		expect(storeState.draft).toContain('Brief for newly scaffolded Skill "release-notes"');
		expect(storeState.draft).toContain('<!-- ikenga:auto -->');
	});
});
