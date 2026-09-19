// WP-20 (frame slot skeleton): the right-side "Companion" dock.
//
// Renders today's Dock verbatim. Owned by WP-09 going forward (per
// `designs/frame-workbench-v4.html`, this is the "Companion" region).
import { Dock } from '@/shell/dock/dock';

export function CompanionSlot() {
	return <Dock />;
}
