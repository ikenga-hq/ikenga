// UI routes registry smoke test.
//
// 1. Install /tmp/test-pkg-com.example.uiroutes (declares /dashboard iframe + /settings component).
// 2. Status → assert registry surfaces both routes namespaced under
//    `pkg://com.example.uiroutes/...`.
// 3. Uninstall → assert registry is empty for the pkg.
//
// The shell is NOT actually mounting the iframe yet — this just proves the
// registry lifecycle and snapshot wiring.
import { createFileRoute, notFound } from '@tanstack/react-router';
import { useEffect, useState } from 'react';

import { pkgInstallFromPath, pkgKernelStatus, pkgUninstall } from '@/lib/tauri-cmd';

export const Route = createFileRoute('/dev/uiroutes-smoke')({
	beforeLoad: () => {
		if (!import.meta.env.DEV) {
			throw notFound();
		}
	},
	component: import.meta.env.DEV ? UiRoutesSmoke : () => null,
});

const PKG_PATH = '/tmp/test-pkg-com.example.uiroutes';
const PKG_ID = 'com.example.uiroutes';

type Row = { label: string; outcome: string };

interface UiEntry {
	pkg_id: string;
	virtual_path: string;
	path: string;
	kind: string;
	source: string;
}

function UiRoutesSmoke() {
	const [rows, setRows] = useState<Row[]>([]);
	const [verdict, setVerdict] = useState('RUNNING');

	useEffect(() => {
		let cancelled = false;
		const log = (label: string, outcome: string) => {
			if (cancelled) return;
			// eslint-disable-next-line no-console
			console.log(`[uiroutes-smoke] ${label}: ${outcome}`);
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
			try {
				const s = await pkgKernelStatus();
				const reg = s.registries.ui_routes as { entries: UiEntry[]; count: number } | undefined;
				const mine = (reg?.entries ?? []).filter((e) => e.pkg_id === PKG_ID);
				const dashboard = mine.find((e) => e.path === '/dashboard');
				const settings = mine.find((e) => e.path === '/settings');
				registryOk =
					mine.length === 2 &&
					!!dashboard &&
					dashboard.kind === 'iframe' &&
					dashboard.virtual_path === `pkg://${PKG_ID}/dashboard` &&
					!!settings &&
					settings.kind === 'component';
				log(
					'REGISTRY',
					`pkg_routes=${mine.length} dashboard=${dashboard ? `${dashboard.kind}:${dashboard.virtual_path}` : 'missing'} settings=${settings ? settings.kind : 'missing'}`
				);
			} catch (e) {
				log('REGISTRY', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			let uninstallOk = false;
			try {
				await pkgUninstall(PKG_ID);
				const s = await pkgKernelStatus();
				const reg = s.registries.ui_routes as { entries: UiEntry[]; count: number } | undefined;
				const mine = (reg?.entries ?? []).filter((e) => e.pkg_id === PKG_ID);
				uninstallOk = mine.length === 0;
				log('UNINSTALL', `OK pkg_routes_after=${mine.length}`);
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
			<h1 style={{ fontSize: 16, marginBottom: 12 }}>Smoke: UI Routes Registry</h1>
			<div data-testid="uiroutes-smoke-verdict" style={{ marginBottom: 16, fontWeight: 'bold' }}>
				{verdict}
			</div>
			<ul style={{ listStyle: 'none', padding: 0 }}>
				{rows.map((r, i) => (
					<li key={i} data-testid={`uiroutes-smoke-row-${i}`}>
						<strong>{r.label}</strong> — {r.outcome}
					</li>
				))}
			</ul>
		</div>
	);
}
