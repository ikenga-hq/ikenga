// plans/pwa Shape 4 (install part): say plainly whether and how this browser
// can install Ikenga — a real Install button where the browser offered a
// prompt, Share-sheet steps on iOS, "needs HTTPS" on plain HTTP — and nothing
// at all on the desktop app or once installed.

import { Download } from 'lucide-react';
import { useState } from 'react';
import { Banner } from '@/components/ui/banner';
import { Button } from '@/components/ui/button';
import {
	installMessage,
	installState,
	isStandaloneDisplay,
	type PlatformEnv,
	tokenSessionWarning,
} from '@/lib/pwa/platform';
import { usePwaStore } from '@/lib/pwa/update-store';
import { getAuthToken, isTauri } from '@/lib/transport';
import { isDeviceSession } from '@/lib/transport/device-session';
import { isT1Session } from '@/lib/transport/t1-session';

function platformEnv(hasInstallPrompt: boolean): PlatformEnv {
	const nav = typeof navigator === 'undefined' ? null : navigator;
	return {
		isTauri: isTauri(),
		secureContext: typeof window !== 'undefined' && window.isSecureContext === true,
		standalone: isStandaloneDisplay(),
		userAgent: nav?.userAgent ?? '',
		platform: nav?.platform ?? '',
		maxTouchPoints: nav?.maxTouchPoints ?? 0,
		hasInstallPrompt,
	};
}

/** A quiet inline hint (the `/remote` client footer). */
export function InstallHint({ className }: { className?: string }) {
	const prompt = usePwaStore((s) => s.installPrompt);
	const setPrompt = usePwaStore((s) => s.setInstallPrompt);
	const [dismissed, setDismissed] = useState(false);
	const state = installState(platformEnv(prompt !== null));
	const message = installMessage(state);
	if (!message || dismissed) return null;

	const install = async () => {
		if (!prompt) return;
		await prompt.prompt();
		const choice = await prompt.userChoice.catch(() => null);
		// A prompt can be shown once; drop it either way.
		setPrompt(null);
		if (choice?.outcome === 'dismissed') setDismissed(true);
	};

	return (
		<div
			data-state={`install-hint-${state}`}
			className={`flex items-start gap-2 text-[length:var(--text-micro)] text-[var(--fg-muted)] ${className ?? ''}`}
		>
			<Download aria-hidden className="mt-0.5 size-3.5 flex-none" />
			<span className="min-w-0 flex-1">{message}</span>
			{state === 'prompt' && (
				<Button size="xs" onClick={() => void install()}>
					Install
				</Button>
			)}
		</div>
	);
}

/**
 * The T0 caveat as an `info` banner: an installed app signed in with a link
 * token forgets it when closed. Points at pairing, whose device cookie lasts.
 */
export function InstalledTokenBanner() {
	const message = tokenSessionWarning({
		standalone: !isTauri() && isStandaloneDisplay(),
		hasBearerToken: !isTauri() && getAuthToken() !== null,
		isDevice: isDeviceSession(),
		isT1: isT1Session(),
	});
	if (!message) return null;
	return (
		<Banner
			data-state="pwa-token-session"
			tone="info"
			icon={<Download />}
			actions={
				<Button size="sm" variant="outline" asChild>
					<a href="/remote/pair">Pair this device</a>
				</Button>
			}
		>
			{message}
		</Banner>
	);
}
