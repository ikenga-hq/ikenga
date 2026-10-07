// plans/pwa S4 §3: the one-time "Get notified on this phone" card in the
// `/remote` client. Shown only where push can actually work; Turn on asks for
// permission inside the click (never on load); Not now is remembered per
// device. On iOS Safari it explains installing first instead.

import { Bell } from 'lucide-react';
import { useEffect, useState } from 'react';

import { Button } from '@/components/ui/button';
import { accessPushConfig } from '@/lib/access/client';
import { currentPushEnv, pushMessage, pushState } from '@/lib/pwa/platform';
import { enablePush, hasBrowserSubscription } from '@/lib/pwa/push-client';
import { isTauri } from '@/lib/transport';

export const NUDGE_STORAGE_KEY = 'ikenga.pushNudge.v1';

function dismissed(): boolean {
	try {
		return localStorage.getItem(NUDGE_STORAGE_KEY) === 'dismissed';
	} catch {
		return false;
	}
}

function dismiss(): void {
	try {
		localStorage.setItem(NUDGE_STORAGE_KEY, 'dismissed');
	} catch {
		// Blocked storage: the card returns next time; nothing breaks.
	}
}

export function PushNudge({ className }: { className?: string }) {
	const [visible, setVisible] = useState(false);
	const [busy, setBusy] = useState(false);
	const [note, setNote] = useState<string | null>(null);
	const state = pushState(currentPushEnv(isTauri()));

	useEffect(() => {
		if (dismissed() || (state !== 'ready' && state !== 'ios-needs-install')) return;
		let live = true;
		void (async () => {
			if (state === 'ready') {
				if (await hasBrowserSubscription()) return;
				const config = await accessPushConfig().catch(() => null);
				if (!config?.enabled) return;
			}
			if (live) setVisible(true);
		})();
		return () => {
			live = false;
		};
	}, [state]);

	if (!visible) return null;
	const close = () => {
		dismiss();
		setVisible(false);
	};
	const turnOn = async () => {
		setBusy(true);
		const r = await enablePush();
		setBusy(false);
		if (r.ok) close();
		else setNote(r.message);
	};

	return (
		<div
			data-state="push-nudge"
			className={`flex items-start gap-2 text-[length:var(--text-micro)] text-[var(--fg-muted)] ${className ?? ''}`}
		>
			<Bell aria-hidden className="mt-0.5 size-3.5 flex-none text-[var(--fg)]" />
			<div className="min-w-0 flex-1 space-y-1.5">
				<div className="font-semibold text-[var(--fg)]">Get notified on this phone</div>
				<div>
					{note ??
						(state === 'ios-needs-install'
							? pushMessage('ios-needs-install')
							: 'Approvals, finished runs and pairing requests, even with Ikenga closed. Only the kind of event is sent.')}
				</div>
				<div className="flex gap-2">
					{state === 'ready' && (
						<Button size="xs" disabled={busy} onClick={() => void turnOn()}>
							Turn on
						</Button>
					)}
					<Button size="xs" variant="outline" onClick={close}>
						Not now
					</Button>
				</div>
			</div>
		</div>
	);
}
