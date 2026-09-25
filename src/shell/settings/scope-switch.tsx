import { cn } from '@/components/ui/utils';
import type { SettingsScopeId } from '@/shell/settings/nav';

interface SettingsScopeSwitchProps {
	scope: SettingsScopeId;
	onScopeChange: (scope: SettingsScopeId) => void;
	/** Project scope needs a filesystem root to write `<root>/.ikenga/`. */
	projectAvailable: boolean;
	ariaLabel?: string;
	className?: string;
}

/**
 * The Personal / Project segmented control. Extracted from
 * `SettingsSectionHeader` so the D-04 consecration's `project` and
 * `equipment` steps render the same primitive the D-03 settings shell does.
 */
export function SettingsScopeSwitch({
	scope,
	onScopeChange,
	projectAvailable,
	ariaLabel = 'Settings scope',
	className,
}: SettingsScopeSwitchProps) {
	// `min-h-[var(--tab-h)]` on each button, not a hardcoded px value —
	// `--tab-h` is the same tab-height token `.ccfg-tab` sizes off of
	// (src/shell/claude-config/claude-config.css) and is the shell's
	// 44px hit-target floor in spacious density (tokens.css
	// `[data-density='spacious']`; D-03 44px targets, WP-35 DoD).
	const buttonClass =
		'min-h-[var(--tab-h)] rounded px-2 py-1 text-xs transition-colors outline-none focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary';
	const stateClass = (on: boolean) =>
		on ? 'bg-card text-foreground shadow-sm' : 'text-muted-foreground hover:text-foreground';
	return (
		<div
			role="group"
			aria-label={ariaLabel}
			className={cn(
				'inline-flex items-center gap-0.5 rounded-md border border-border p-0.5',
				className
			)}
		>
			<button
				type="button"
				aria-pressed={scope === 'personal'}
				onClick={() => onScopeChange('personal')}
				data-scope="personal"
				className={cn(buttonClass, stateClass(scope === 'personal'))}
			>
				Personal
			</button>
			<button
				type="button"
				aria-pressed={scope === 'project'}
				disabled={!projectAvailable}
				title={projectAvailable ? undefined : 'The active project has no filesystem root'}
				onClick={() => onScopeChange('project')}
				data-scope="project"
				className={cn(
					buttonClass,
					'disabled:cursor-not-allowed disabled:opacity-50',
					stateClass(scope === 'project')
				)}
			>
				Project
			</button>
		</div>
	);
}
