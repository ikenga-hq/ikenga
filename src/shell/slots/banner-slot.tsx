// WP-20 (frame slot skeleton): banner stack.
//
// Renders today's banner stack verbatim (unchanged order/props). A later WP
// may collapse this into the design's single-banner-with-"+N more" slot
// (P8), but that is out of scope here — this is a no-op extraction only.
import { ConnectionBanner } from '@/shell/connection-banner';
import { ConnectorBanner } from '@/shell/connector-banner';
import { PkgAutoUpdater } from '@/shell/pkg-auto-updater';
import { TrustReviewBanner } from '@/shell/trust-review-banner';
import { UpdaterBanner } from '@/shell/updater-banner';

export function BannerSlot() {
	return (
		<>
			<ConnectionBanner />
			<UpdaterBanner />
			<PkgAutoUpdater />
			<ConnectorBanner />
			<TrustReviewBanner />
		</>
	);
}
