// "Clear local data" button + the shared localStorage-key catalog the
// layout-reset button in Settings → Appearance also reads. Wipes the SQLite
// cache, browser localStorage, AND the durable settings_kv mirror so a user
// who clicks here gets a true clean slate (auth + theme survive).

import { confirm as confirmDialog } from '@/lib/transport/dialog-shim';
import { AlertTriangle, Trash2 } from 'lucide-react';
import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { loadAppDb } from '@/lib/sql-db';
import { settingsClearAll } from '@/lib/tauri-cmd';

// Persistence keys this UI knows how to clear. Kept in one place so adding a
// new persisted store means adding it here too — the button is otherwise a
// footgun. Also exported for the Reset-Layout button in
// `routes/settings/appearance.tsx`.
export const KNOWN_LOCALSTORAGE_KEYS = [
	'shell-store',
	'ikenga-dock',
	'ikenga-shell',
	'entity-store',
	'terminal.tabs',
	'__boot_timings__',
];

export const LAYOUT_LS_PREFIX = '__lstate__:';
export const REVEAL_TIMEOUT_MS = 30_000;

export function ClearDataSectionBody() {
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<string | null>(null);

	async function handleClear() {
		const ok = await confirmDialog(
			'This wipes locally cached app state: terminal sessions, viewer recents, render queue, mbox sync, storyboards, layout, dock, sequences, browser localStorage, AND the durable settings_kv mirror (theme/agent/onboarding state). Stronghold secrets, screenshots on disk, and your Supabase data are kept. The app will reload.',
			{ title: 'Clear all local data', kind: 'warning' }
		);
		if (!ok) return;
		setBusy(true);
		setError(null);
		try {
			// SQLite content tables.
			try {
				const db = await loadAppDb();
				const tables = [
					'layout_state',
					'viewer_recents',
					'claude_sessions',
					'render_queue',
					'mbox_sync_state',
					'storyboards',
					'storyboard_beats',
					'storyboard_jobs',
				];
				for (const t of tables) {
					try {
						await db.execute(`DELETE FROM ${t}`);
					} catch (e) {
						console.warn(`[settings] DELETE FROM ${t} failed`, e);
					}
				}
			} catch (e) {
				console.warn('[settings] sqlite unavailable for clear', e);
			}

			// settings_kv mirror — wipe it too so the user gets a truly fresh
			// state on reload. Without this, the agent/onboarding state would
			// survive because hydrateSettingsFromRust pulls them back in.
			try {
				await settingsClearAll();
			} catch (e) {
				console.warn('[settings] settings_clear_all failed', e);
			}

			// localStorage: nuke everything except auth/theme so the user isn't
			// booted out and the app reopens looking the same.
			const keep = new Set(['ikenga-shell']);
			const all: string[] = [];
			for (let i = 0; i < localStorage.length; i++) {
				const k = localStorage.key(i);
				if (k) all.push(k);
			}
			for (const k of all) {
				if (!keep.has(k) && !k.startsWith('sb-')) localStorage.removeItem(k);
			}
			for (const k of KNOWN_LOCALSTORAGE_KEYS) {
				if (k !== 'ikenga-shell') localStorage.removeItem(k);
			}

			window.location.reload();
		} catch (e) {
			setError(String(e));
		} finally {
			setBusy(false);
		}
	}

	return (
		<div className="space-y-3 px-4 py-3">
			<p className="text-xs text-muted-foreground">
				Destructive. Wipes the SQLite cache, browser localStorage, and the settings_kv mirror. Auth
				and theme are preserved; Supabase data is untouched.
			</p>
			<div className="rounded-md border border-amber-300/60 bg-amber-50/40 p-3 text-xs text-amber-900 dark:border-amber-900/60 dark:bg-amber-950/20 dark:text-amber-200">
				<div className="flex items-start gap-2">
					<AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
					<span>
						This does not touch Stronghold secrets or screenshot PNGs already written to disk.
					</span>
				</div>
			</div>
			<div>
				<Button
					variant="outline"
					size="sm"
					onClick={handleClear}
					disabled={busy}
					className="text-red-700"
				>
					<Trash2 className="mr-1 h-3.5 w-3.5" />
					Clear all local data
				</Button>
				{error && <p className="mt-2 text-xs text-red-700">{error}</p>}
			</div>
		</div>
	);
}
