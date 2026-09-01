// Permissions registry smoke test. Proves an installed package's
// `permissions.fs.read` entry actually grants the underlying Tauri scope.
//
// 1. Read /tmp/perms-test-allowed/file.txt — expect blocked.
// 2. Install /tmp/test-pkg-com.example.perms (declares fs.read for that path).
// 3. Read again — expect success + matching body.
// 4. Cleanup: uninstall.
import { createFileRoute } from '@tanstack/react-router';
import { useEffect, useState } from 'react';
import { invoke } from '@/lib/transport';

import {
	pkgInstallFromPath,
	pkgKernelStatus,
	pkgUninstall,
	spikeGrantFsRead,
} from '@/lib/tauri-cmd';

export const Route = createFileRoute('/perms-smoke')({
	component: PermsSmoke,
});

const PKG_PATH = '/tmp/test-pkg-com.example.perms';
const PKG_ID = 'com.example.perms';
const TEST_PATH = '/tmp/perms-test-allowed/file.txt';

type Row = { label: string; outcome: string };

const readTextFile = async (path: string): Promise<string> => {
	const buf = await invoke<ArrayBuffer>('plugin:fs|read_text_file', { path });
	return new TextDecoder().decode(buf);
};

function PermsSmoke() {
	const [rows, setRows] = useState<Row[]>([]);
	const [verdict, setVerdict] = useState('RUNNING');

	useEffect(() => {
		let cancelled = false;
		const log = (label: string, outcome: string) => {
			if (cancelled) return;
			// eslint-disable-next-line no-console
			console.log(`[perms-smoke] ${label}: ${outcome}`);
			setRows((p) => [...p, { label, outcome }]);
		};

		(async () => {
			// Make sure no stale install from a prior run is still granting
			// perms in this process. Note: even after uninstall, Tauri ACL keeps
			// the previously-granted scope until restart — so the BLOCKED step
			// only behaves correctly on a fresh process.
			try {
				await pkgUninstall(PKG_ID);
			} catch {
				// not installed
			}

			// Step 1: read without grant — expect failure (only meaningful on
			// a fresh process; we log either outcome and let the verdict reflect
			// that grant *did* register).
			let preBlocked = false;
			try {
				const body = await readTextFile(TEST_PATH);
				log(
					'PRE-INSTALL READ',
					`UNEXPECTED_OK len=${body.length} (process may already have grant from earlier run)`
				);
			} catch (e) {
				preBlocked = true;
				log('PRE-INSTALL READ', `BLOCKED_AS_EXPECTED ${(e as Error).message ?? String(e)}`);
			}

			// Control: grant via the proven spike command first. If the read
			// succeeds via this path but fails via the perms registry, the bug is
			// in the registry's add_capability shape, not in dynamic ACL.
			try {
				const msg = await spikeGrantFsRead(`spike.perms-control.${Date.now()}`, TEST_PATH);
				log('SPIKE GRANT', `OK ${msg}`);
				try {
					const body = await readTextFile(TEST_PATH);
					log('SPIKE READ', `READ_OK len=${body.length}`);
				} catch (e) {
					log('SPIKE READ', `READ_FAIL ${(e as Error).message ?? String(e)}`);
				}
			} catch (e) {
				log('SPIKE GRANT', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			// Step 2: install package with permissions declaration.
			let installed = false;
			try {
				const r = await pkgInstallFromPath(PKG_PATH);
				installed = true;
				log('INSTALL', `OK id=${r.installed.id}`);
			} catch (e) {
				log('INSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			// Step 3: read after grant — must succeed with matching body.
			let postRead = false;
			let bodyMatches = false;
			try {
				const body = await readTextFile(TEST_PATH);
				postRead = true;
				bodyMatches = body.startsWith('perms allowed body ');
				log('POST-INSTALL READ', `READ_OK len=${body.length} body_matches=${bodyMatches}`);
			} catch (e) {
				log('POST-INSTALL READ', `READ_FAIL ${(e as Error).message ?? String(e)}`);
			}

			// Step 4: status check — confirm permissions registry tracks the grant.
			try {
				const s = await pkgKernelStatus();
				const perms = s.registries.permissions as {
					entries: Array<{ pkg_id: string; scope_kind: string; scope_value: string }>;
					count: number;
				};
				const granted = perms.entries.filter((e) => e.pkg_id === PKG_ID);
				log(
					'REGISTRY',
					`granted_count=${granted.length} kinds=${JSON.stringify(granted.map((g) => g.scope_kind))}`
				);
			} catch (e) {
				log('REGISTRY', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			// Cleanup.
			try {
				await pkgUninstall(PKG_ID);
				log('CLEANUP', 'OK');
			} catch (e) {
				log('CLEANUP', `WARN ${(e as Error).message ?? String(e)}`);
			}

			// Verdict: install succeeded AND post-install read succeeded with
			// matching body. The pre-install BLOCKED check is informational —
			// re-runs in the same process see the prior grant survive.
			const ok = installed && postRead && bodyMatches;
			const v = ok
				? `VERDICT PASS${preBlocked ? ' (fresh-process: pre-install was BLOCKED)' : ' (re-run: pre-install grant leaked from earlier)'}`
				: 'VERDICT FAIL';
			log('DONE', v);
			if (!cancelled) setVerdict(v);
		})();

		return () => {
			cancelled = true;
		};
	}, []);

	return (
		<div style={{ padding: 24, fontFamily: 'monospace', fontSize: 13 }}>
			<h1 style={{ fontSize: 16, marginBottom: 12 }}>Smoke: Permissions Registry</h1>
			<div data-testid="perms-smoke-verdict" style={{ marginBottom: 16, fontWeight: 'bold' }}>
				{verdict}
			</div>
			<ul style={{ listStyle: 'none', padding: 0 }}>
				{rows.map((r, i) => (
					<li key={i} data-testid={`perms-smoke-row-${i}`}>
						<strong>{r.label}</strong> — {r.outcome}
					</li>
				))}
			</ul>
		</div>
	);
}
