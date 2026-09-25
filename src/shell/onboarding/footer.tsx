// D-04 footer — `designs/onboarding.html` `<footer class="wfoot">`: the
// "Writes" mono line, the `iyke go` CLI equivalent, "Everything here can be
// changed in Settings", and Back · Skip · Continue.
//
// `WRITES` / `IYKE` / `SETTINGS_LINKS` are keyed by the new D-04 step ids —
// they are this file's single source of truth for "what does this step
// write and where can I change it later", consumed by both the footer and
// each step body's inline `<WritesNote>` (mirrors the mock rendering the
// same note in two places: `writesBox(step)` inline in the column, and
// `$('#footWrites')` in the footer).

import { Link } from '@tanstack/react-router';

import { Button } from '@/components/ui/button';
import type { OnboardingStepId } from '@/lib/shell/shell-store';

export interface WriteInfo {
	/** Mono path shown to the user. Only `settings.json` paths are backed by
	 *  a real "Open file" action today (`openSettingsFile`) — the others
	 *  (pkgs dir, `.claude/`, keybindings.json) have no generic "open a
	 *  path" command exposed to onboarding yet, so their button is omitted.
	 *  Flagged in the WP-38 PR body as a gap, not silently faked. */
	file: string;
	note: string;
	openable: boolean;
}

// Per-step "what this step writes" — mirrors `designs/onboarding.html`
// `WRITES`. `equipment` and `shortcuts` write outside settings.json (pkgs +
// `.claude/`, and keybindings respectively), so `openable: false` there.
export const WRITES: Record<OnboardingStepId, WriteInfo> = {
	welcome: {
		file: '~/.ikenga/settings.json',
		note: 'Records your name and that the consecration started.',
		openable: true,
	},
	engine: {
		file: '~/.ikenga/settings.json',
		note: 'Writes engines.defaultEngineId and (if chosen) installs the offline engine pkg.',
		openable: true,
	},
	project: {
		file: '~/.ikenga/settings.json',
		note: 'Adds the project and its extra roots.',
		openable: true,
	},
	equipment: {
		file: '~/.ikenga/pkgs/ + <project>/.claude/',
		note: 'Installs packages; adopts or scaffolds what is already on disk. Connector keys go to the vault, never to a file.',
		openable: false,
	},
	look: {
		file: '~/.ikenga/settings.json',
		note: 'Writes appearance.theme, .mode and .density.',
		openable: true,
	},
	shortcuts: {
		file: '~/.ikenga/keybindings.json',
		note: 'Nothing is written unless you rebind a key.',
		openable: false,
	},
	done: {
		file: '~/.ikenga/settings.json',
		note: 'Stamps the consecration complete.',
		openable: true,
	},
};

// The CLI equivalent each step's footer line names (D-04 principle P5:
// "every step carries its CLI equivalent in the footer's mono line").
export const IYKE: Record<OnboardingStepId, string> = {
	welcome: 'iyke go /onboarding/welcome',
	engine: 'iyke go /onboarding/engine',
	project: 'iyke go /onboarding/project',
	equipment: 'iyke go /onboarding/equipment',
	look: 'iyke go /onboarding/look',
	shortcuts: 'iyke go /onboarding/shortcuts',
	done: 'iyke go /onboarding/done',
};

export interface SettingsLink {
	label: string;
	to: string;
}

// "Everything here can be changed in Settings" — resolved per D-03 section.
// Carried onto the v0.13.0 (WP-35) settings shell: all links below now point
// directly at the real D-03 section routes rather than the pre-WP-35 names
// they were flagged against in the original PR body (`/settings/agent` and
// `/settings/backup` still exist as redirect stubs to `engines`/`storage`,
// but there's no reason to bounce through them here).
export const SETTINGS_LINKS: Partial<Record<OnboardingStepId, SettingsLink[]>> = {
	engine: [{ label: 'Settings · Chi & engines', to: '/settings/engines' }],
	project: [{ label: 'Settings · Projects', to: '/settings/projects' }],
	equipment: [
		{ label: 'Ngwa', to: '/ngwa' },
		{ label: 'Settings · Integrations', to: '/settings/integrations' },
	],
	look: [{ label: 'Settings · Appearance', to: '/settings/appearance' }],
	// `shortcuts` has no settings surface yet — keybinding editing is a
	// Phase 6 (D-06) deliverable. `welcome` and `done` are framing steps
	// with nothing of their own to re-open.
};

export function WritesNote({
	stepId,
	onOpenFile,
	file,
}: {
	stepId: OnboardingStepId;
	/** Overrides `WRITES[stepId].file` where the D-04 scope switch moves the
	 *  write (`project`, `equipment`). */
	file?: string;
	/** Steps whose `WRITES[stepId].openable` is true pass a handler — usually
	 *  `() => void openSettingsFile('personal').catch(() => {})`. Omitted (or
	 *  a no-op) for `equipment`/`shortcuts`, which write outside settings.json
	 *  and have no generic "open this path" command exposed to onboarding. */
	onOpenFile?: () => void;
}) {
	const w = WRITES[stepId];
	return (
		<div
			className="mt-6 flex flex-wrap items-center gap-2 rounded-md border px-3 py-2 text-xs"
			style={{ borderColor: 'var(--border-soft)', color: 'var(--fg-muted)' }}
			data-testid="onboarding-writes"
		>
			<span style={{ color: 'var(--fg-faint)' }}>Writes</span>
			<span className="font-mono" style={{ color: 'var(--fg)' }}>
				{file ?? w.file}
			</span>
			<span className="flex-1">{w.note}</span>
			{w.openable && onOpenFile && (
				<Button
					variant="ghost"
					size="sm"
					className="h-6 px-2 text-[11px]"
					onClick={onOpenFile}
					data-testid="onboarding-writes-open"
				>
					Open file
				</Button>
			)}
		</div>
	);
}

interface OnboardingFooterProps {
	stepId: OnboardingStepId;
	isFirst: boolean;
	isLast: boolean;
	isOptional: boolean;
	progressLabel: string;
	onBack: () => void;
	onSkip: () => void;
	onNext: () => void;
}

export function OnboardingFooter({
	stepId,
	isFirst,
	isLast,
	isOptional,
	progressLabel,
	onBack,
	onSkip,
	onNext,
}: OnboardingFooterProps) {
	const links = SETTINGS_LINKS[stepId];
	return (
		<footer
			className="flex flex-col gap-2 border-t px-12 py-4"
			style={{ borderColor: 'var(--border-soft)', background: 'var(--bg-surface)' }}
		>
			<div className="flex items-center justify-between gap-4">
				<div className="flex min-w-0 flex-wrap items-center gap-x-4 gap-y-1">
					<span className="font-mono text-xs" style={{ color: 'var(--fg-faint)' }}>
						{progressLabel}
					</span>
					<span className="font-mono text-[11px]" style={{ color: 'var(--fg-faint)' }}>
						{IYKE[stepId]}
					</span>
					{!isFirst &&
						!isLast &&
						(links ?? []).map((link) => (
							<span key={link.to} className="text-[11px]" style={{ color: 'var(--fg-faint)' }}>
								Change later ·{' '}
								<Link
									to={link.to}
									className="underline-offset-2 hover:underline"
									style={{ color: 'var(--primary)' }}
								>
									{link.label}
								</Link>
							</span>
						))}
				</div>
				<div className="flex flex-none items-center gap-3">
					{isOptional && !isLast && (
						<Button variant="ghost" onClick={onSkip} data-testid="wizard-skip" className="h-9">
							Skip
						</Button>
					)}
					{!isFirst && (
						<Button variant="ghost" onClick={onBack} data-testid="wizard-back" className="h-9">
							Back
						</Button>
					)}
					<Button
						onClick={onNext}
						data-testid="wizard-next"
						className="h-11 px-6 text-sm font-semibold"
					>
						{isLast ? 'Enter your Obi' : 'Continue'}
					</Button>
				</div>
			</div>
		</footer>
	);
}
