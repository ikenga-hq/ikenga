import { useState, useEffect } from 'react';
import { useReauthStore } from '@/lib/transport/reauth-store';

export function ReauthOverlay() {
	const isOpen = useReauthStore((s) => s.isOpen);
	const tokenInput = useReauthStore((s) => s.tokenInput);
	const setTokenInput = useReauthStore((s) => s.setTokenInput);
	const errorMsg = useReauthStore((s) => s.errorMsg);
	const reconnect = useReauthStore((s) => s.reconnect);

	const [timeStr, setTimeStr] = useState<string>('');
	const [loading, setLoading] = useState<boolean>(false);

	// Gated on `isOpen`. Hooks run before the `!isOpen` early return below, so
	// an ungated interval ticks and re-renders this component once a second
	// for the entire lifetime of every window, overlay shown or not.
	useEffect(() => {
		if (!isOpen) return;
		const updateTime = () => {
			const d = new Date();
			setTimeStr(d.toTimeString().split(' ')[0] || '');
		};
		updateTime();
		const interval = setInterval(updateTime, 1000);
		return () => clearInterval(interval);
	}, [isOpen]);

	if (!isOpen) return null;

	const handleReconnect = async () => {
		setLoading(true);
		try {
			await reconnect(tokenInput);
		} finally {
			setLoading(false);
		}
	};

	return (
		<div className="fixed inset-0 z-50 grid place-items-center bg-[color-mix(in_srgb,var(--bg-base)_78%,transparent)] p-6 backdrop-blur-xs">
			<div className="w-full max-w-[460px] overflow-hidden rounded-xl border border-[var(--border-strong)] bg-[var(--bg-surface)] text-[var(--fg)] shadow-2xl">
				{/* Top bar */}
				<div className="flex items-center gap-2.5 border-b border-[var(--border-soft)] bg-[var(--danger-soft)] px-5 py-4">
					<span className="h-2 w-2 flex-none rounded-full bg-[var(--danger)]" />
					<h2 className="m-0 text-[var(--text-h4)] font-semibold">
						Session needs re-authenticating
					</h2>
					<span className="ml-auto font-mono text-[var(--text-micro)] text-[var(--fg-faint)]">
						{timeStr}
					</span>
				</div>

				{/* Body */}
				<div className="p-5">
					<p className="mb-4 text-[var(--text-body-sm)] text-[var(--fg-muted)] leading-relaxed">
						The daemon restarted and minted a new token, so this tab's saved one no longer works.
						Your work is untouched — paste the current token to pick it back up.
					</p>

					<div className="flex gap-2">
						<input
							type="password"
							value={tokenInput}
							onChange={(e) => setTokenInput(e.target.value)}
							placeholder="Paste auth token..."
							className="flex-1 rounded-md border border-[var(--border)] bg-[var(--bg-sunken)] px-3 py-2 font-mono text-[var(--text-body-sm)] text-[var(--fg)] outline-none focus:border-[var(--primary)] focus:ring-2 focus:ring-[var(--primary-soft)]"
							autoFocus
							spellCheck={false}
							onKeyDown={(e) => {
								if (e.key === 'Enter') {
									e.preventDefault();
									handleReconnect();
								}
							}}
						/>
						<button
							type="button"
							onClick={handleReconnect}
							disabled={loading}
							className="rounded-md bg-[var(--primary)] px-5 py-2 font-semibold text-[var(--text-body-sm)] text-[var(--primary-fg)] hover:opacity-90 disabled:opacity-50 cursor-pointer"
						>
							{loading ? 'Connecting...' : 'Reconnect'}
						</button>
					</div>

					{errorMsg && (
						<div className="mt-3 font-mono text-[12px] text-[var(--danger)]">{errorMsg}</div>
					)}

					<div className="mt-4 border-t border-[var(--border-soft)] pt-4 text-[var(--text-micro)] text-[var(--fg-faint)] leading-relaxed">
						<b className="text-[var(--live)] font-semibold">Still running on the host</b> — session
						active. Nothing is lost by reconnecting.
					</div>
				</div>
			</div>
		</div>
	);
}
