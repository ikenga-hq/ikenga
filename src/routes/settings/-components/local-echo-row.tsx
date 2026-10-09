import { SettingsFieldRow } from '@/shell/settings/field';
import type { LocalEchoMode } from '@/terminal/local-echo/engine';
import { useLocalEchoSettings } from '@/terminal/local-echo/settings';

const OPTIONS: { value: LocalEchoMode; label: string }[] = [
	{ value: 'auto', label: 'Auto (above 80 ms)' },
	{ value: 'always', label: 'Always' },
	{ value: 'off', label: 'Off' },
];

/**
 * Predictive local echo (browser only — the desktop's terminals are local and
 * never predict). Stored per browser: the right answer depends on this
 * device's connection, not on the workspace.
 */
export function LocalEchoSettingsRow() {
	const mode = useLocalEchoSettings((s) => s.mode);
	const setMode = useLocalEchoSettings((s) => s.setMode);
	return (
		<SettingsFieldRow
			field={null}
			label="Predictive local echo"
			desc="Show typed characters immediately, before the server echoes them, then confirm or roll them back. Auto turns it on when this browser's round trip to the server is above 80 ms. Applies to this browser only."
		>
			<select
				aria-label="Predictive local echo"
				value={mode}
				onChange={(e) => setMode(e.target.value as LocalEchoMode)}
				className="rounded-md border border-border bg-background px-3 py-1.5 text-xs font-medium text-foreground outline-none focus:ring-1 focus:ring-primary"
			>
				{OPTIONS.map((o) => (
					<option key={o.value} value={o.value}>
						{o.label}
					</option>
				))}
			</select>
		</SettingsFieldRow>
	);
}
