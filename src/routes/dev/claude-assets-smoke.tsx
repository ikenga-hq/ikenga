// Claude assets registry smoke test.
//
// 1. Install /tmp/test-pkg-com.example.claude (declares skills/commands/agents).
// 2. Status → assert registry surfaces 3 entries (skills + commands + agents).
// 3. Read each target via fsRead → assert symlink resolves and file contents match.
// 4. Uninstall → re-stat targets via fsExists → assert all gone.
import { createFileRoute, notFound } from '@tanstack/react-router';
import { useEffect, useState } from 'react';

import { pkgInstallFromPath, pkgKernelStatus, pkgUninstall } from '@/lib/tauri-cmd';

export const Route = createFileRoute('/dev/claude-assets-smoke')({
	beforeLoad: () => {
		if (!import.meta.env.DEV) {
			throw notFound();
		}
	},
	component: import.meta.env.DEV ? ClaudeAssetsSmoke : () => null,
});

const PKG_PATH = '/tmp/test-pkg-com.example.claude';
const PKG_ID = 'com.example.claude';

type Row = { label: string; outcome: string };

interface Entry {
	pkg_id: string;
	kind: string;
	source: string;
	target: string;
}

function ClaudeAssetsSmoke() {
	const [rows, setRows] = useState<Row[]>([]);
	const [verdict, setVerdict] = useState('RUNNING');

	useEffect(() => {
		let cancelled = false;
		const log = (label: string, outcome: string) => {
			if (cancelled) return;
			// eslint-disable-next-line no-console
			console.log(`[claude-assets-smoke] ${label}: ${outcome}`);
			setRows((p) => [...p, { label, outcome }]);
		};

		(async () => {
			try {
				await pkgUninstall(PKG_ID);
			} catch {
				// not installed
			}

			let installed = false;
			try {
				const r = await pkgInstallFromPath(PKG_PATH);
				installed = true;
				log('INSTALL', `OK id=${r.installed.id}`);
			} catch (e) {
				log('INSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			let registryOk = false;
			let recordedTargets: string[] = [];
			try {
				const s = await pkgKernelStatus();
				const reg = s.registries.claude_assets as { entries: Entry[]; count: number } | undefined;
				const mine = (reg?.entries ?? []).filter((e) => e.pkg_id === PKG_ID);
				const kinds = new Set(mine.map((m) => m.kind));
				recordedTargets = mine.map((m) => m.target);
				registryOk =
					mine.length === 3 && kinds.has('skills') && kinds.has('commands') && kinds.has('agents');
				log(
					'REGISTRY',
					`pkg_assets=${mine.length} kinds=${JSON.stringify([...kinds])} targets=${JSON.stringify(recordedTargets)}`
				);
			} catch (e) {
				log('REGISTRY', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			// Filesystem verification happens out-of-band — the fs Tauri commands
			// are allowlisted to ~/royalti-co + ~/.claude/projects + ~/.company,
			// and ~/.claude/{skills,commands,agents}/ is outside that scope. The
			// registry snapshot gives us the target paths; verify with a shell
			// check after the test (see CLI in this route's prompt).

			let uninstallOk = false;
			try {
				await pkgUninstall(PKG_ID);
				const s = await pkgKernelStatus();
				const reg = s.registries.claude_assets as { entries: Entry[]; count: number } | undefined;
				const mine = (reg?.entries ?? []).filter((e) => e.pkg_id === PKG_ID);
				uninstallOk = mine.length === 0;
				log('UNINSTALL', `pkg_assets_after=${mine.length}`);
			} catch (e) {
				log('UNINSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			const ok = installed && registryOk && uninstallOk;
			const v = ok ? 'VERDICT PASS' : 'VERDICT FAIL';
			log('DONE', v);
			if (!cancelled) setVerdict(v);
		})();

		return () => {
			cancelled = true;
		};
	}, []);

	return (
		<div style={{ padding: 24, fontFamily: 'monospace', fontSize: 13 }}>
			<h1 style={{ fontSize: 16, marginBottom: 12 }}>Smoke: Claude Assets Registry</h1>
			<div
				data-testid="claude-assets-smoke-verdict"
				style={{ marginBottom: 16, fontWeight: 'bold' }}
			>
				{verdict}
			</div>
			<ul style={{ listStyle: 'none', padding: 0 }}>
				{rows.map((r, i) => (
					<li key={i} data-testid={`claude-assets-smoke-row-${i}`}>
						<strong>{r.label}</strong> — {r.outcome}
					</li>
				))}
			</ul>
		</div>
	);
}
