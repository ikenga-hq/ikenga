// plans/pwa S1 (W2): "A new version of Ikenga is ready. Reload to update."
//
// Shown when a new service worker has installed and is waiting (see
// `src/lib/pwa/register.ts`). The `update` tier of the banner slot, and at the
// top of the `/remote` client. Never renders under Tauri: the store is only
// written by `registerServiceWorker`, which refuses there.
//
// A waiting update must be hard to miss, so this is mounted by `workspace.tsx`
// above the title row and OUTSIDE the one-at-a-time banner slot (where it used
// to lose to every higher-priority notice). It has no dismiss control and goes
// away only when the page reloads onto the new version.

import { RefreshCw } from 'lucide-react';
import { Banner } from '@/components/ui/banner';
import { Button } from '@/components/ui/button';
import { applyUpdate } from '@/lib/pwa/register';
import { usePwaStore } from '@/lib/pwa/update-store';

export function PwaUpdateBanner() {
	const waiting = usePwaStore((s) => s.waiting);
	if (!waiting) return null;
	return (
		<Banner
			data-state="pwa-update"
			tone="warning"
			icon={<RefreshCw />}
			actions={
				<Button size="sm" onClick={() => applyUpdate()}>
					Reload to update
				</Button>
			}
		>
			<span className="font-medium">A new version of Ikenga is ready.</span> This tab keeps running
			the old one until you reload.
		</Banner>
	);
}
