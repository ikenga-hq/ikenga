// plans/pwa S1 (W2): "A new version of Ikenga is ready. Reload to update."
//
// Shown when a new service worker has installed and is waiting (see
// `src/lib/pwa/register.ts`). The `update` tier of the banner slot, and at the
// top of the `/remote` client. Never renders under Tauri: the store is only
// written by `registerServiceWorker`, which refuses there.

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
			tone="info"
			icon={<RefreshCw />}
			actions={
				<Button size="sm" onClick={() => applyUpdate()}>
					Reload to update
				</Button>
			}
		>
			A new version of Ikenga is ready.
		</Banner>
	);
}
