// Workspace banner for an unreadable saved terminal list. While it's up,
// terminal saving is paused (session-store `restoreError.holdsSave`) so the
// saved list isn't overwritten — so it must be visible app-wide, not only in
// the Explorer's Sessions section, or a user who never opens that section
// would lose every tab opened this session without being told.

import { TerminalSquare } from 'lucide-react';

import { Banner } from '@/components/ui/banner';
import { Button } from '@/components/ui/button';
import { useTerminalStore } from '@/terminal/session-store';

export function TerminalRestoreBanner() {
	const restoreError = useTerminalStore((s) => s.restoreError);
	const dismiss = useTerminalStore((s) => s.dismissRestoreError);
	if (!restoreError?.holdsSave) return null;
	return (
		<Banner
			tone="danger"
			icon={<TerminalSquare />}
			data-testid="terminal-restore-banner"
			actions={
				<Button size="sm" onClick={dismiss} data-testid="terminal-restore-banner-resume">
					Resume saving
				</Button>
			}
		>
			{restoreError.message}
		</Banner>
	);
}
