// WP-20 (frame slot skeleton): the right-hand Companion region.
//
// Filled by WP-06 (per `designs/frame-workbench-v4.html`, D-01, this is the
// Chi Companion — dispatch bar, session tabs, state panels; ADR-021). WP-06
// owns this slot, not WP-09.
import { Companion } from '@/shell/companion/companion';

export function CompanionSlot() {
	return <Companion />;
}
