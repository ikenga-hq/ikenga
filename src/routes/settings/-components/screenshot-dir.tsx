// Screenshot directory — where the `screenshot_window` / `screenshot_pane`
// commands and the iyke CLI bridge save PNGs. Backed by the Rust
// `screenshot::config` state; persisted to disk by Tauri.

import { useEffect, useState } from 'react';
import { open as openDialog } from '@/lib/transport/dialog-shim';
import { Camera, FolderPlus, RotateCcw } from 'lucide-react';

import { Button } from '@/components/ui/button';
import {
	type ScreenshotConfig as ScreenshotCfg,
	screenshotGetConfig,
	screenshotSetDir,
} from '@/lib/tauri-cmd';

export function ScreenshotDirSectionBody() {
	const [cfg, setCfg] = useState<ScreenshotCfg | null>(null);
	const [error, setError] = useState<string | null>(null);
	const [busy, setBusy] = useState(false);

	useEffect(() => {
		let cancelled = false;
		screenshotGetConfig()
			.then((c) => {
				if (!cancelled) setCfg(c);
			})
			.catch((e) => {
				if (!cancelled) setError(String(e));
			});
		return () => {
			cancelled = true;
		};
	}, []);

	async function handleChange() {
		setBusy(true);
		setError(null);
		try {
			const picked = await openDialog({ directory: true, multiple: false });
			if (typeof picked === 'string') {
				await screenshotSetDir(picked);
				setCfg(await screenshotGetConfig());
			}
		} catch (e) {
			setError(String(e));
		} finally {
			setBusy(false);
		}
	}

	async function handleReset() {
		setBusy(true);
		setError(null);
		try {
			await screenshotSetDir(null);
			setCfg(await screenshotGetConfig());
		} catch (e) {
			setError(String(e));
		} finally {
			setBusy(false);
		}
	}

	return (
		<div className="space-y-3 px-4 py-3">
			<p className="text-xs text-muted-foreground">
				Where <code>screenshot_window</code> / <code>screenshot_pane</code> save PNGs (also used by
				the global Ctrl+Alt+Shift+S/P shortcuts and the iyke CLI bridge).
			</p>
			<div className="rounded-md border border-border bg-background p-3 text-sm">
				<div className="flex items-start gap-2">
					<Camera className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" />
					<div className="min-w-0 flex-1 space-y-1">
						<div className="font-mono text-xs break-all">{cfg?.effectiveDir ?? 'Loading…'}</div>
						{cfg && (
							<div className="text-[11px] text-muted-foreground">
								{cfg.overrideDir
									? `Custom override (default: ${cfg.defaultDir})`
									: 'Platform default'}
							</div>
						)}
					</div>
				</div>
			</div>
			<div className="flex gap-2">
				<Button variant="outline" size="sm" onClick={handleChange} disabled={busy}>
					<FolderPlus className="mr-1 h-3.5 w-3.5" />
					Change directory…
				</Button>
				<Button
					variant="ghost"
					size="sm"
					onClick={handleReset}
					disabled={busy || !cfg?.overrideDir}
				>
					<RotateCcw className="mr-1 h-3.5 w-3.5" />
					Reset to default
				</Button>
			</div>
			{error && <p className="text-xs text-red-700">{error}</p>}
		</div>
	);
}
