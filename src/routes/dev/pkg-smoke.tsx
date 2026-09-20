// Smoke test for the pkg kernel.
//
// Default mode (no `?phase=`): install → status → uninstall round-trip.
// `?phase=install`: install only, leave persisted (test boot replay).
// `?phase=verify`:  no-install; just check status reflects prior install.
// `?phase=cleanup`: uninstall only.
import { createFileRoute, notFound } from '@tanstack/react-router';
import { useEffect, useState } from 'react';

import { pkgInstallFromPath, pkgKernelStatus, pkgUninstall } from '@/lib/tauri-cmd';

type Phase = 'roundtrip' | 'install' | 'verify' | 'cleanup';

export const Route = createFileRoute('/dev/pkg-smoke')({
	beforeLoad: () => {
		if (!import.meta.env.DEV) {
			throw notFound();
		}
	},
	component: import.meta.env.DEV ? PkgSmoke : () => null,
	validateSearch: (s: Record<string, unknown>): { phase?: Phase } => ({
		phase: (s.phase as Phase) || undefined,
	}),
});

const PKG_PATH = '/tmp/test-pkg-com.example.demo';
const PKG_ID = 'com.example.demo';

type Row = { label: string; outcome: string };

function PkgSmoke() {
	const search = Route.useSearch();
	const phase: Phase = search.phase ?? 'roundtrip';
	const [rows, setRows] = useState<Row[]>([]);
	const [verdict, setVerdict] = useState('RUNNING');

	useEffect(() => {
		let cancelled = false;
		const log = (label: string, outcome: string) => {
			if (cancelled) return;
			// eslint-disable-next-line no-console
			console.log(`[pkg-smoke] ${label}: ${outcome}`);
			setRows((p) => [...p, { label, outcome }]);
		};

		const checkStatus = async () => {
			const s = await pkgKernelStatus();
			const sidecars = s.registries.sidecars as {
				entries: Array<{ pkg_id: string; name: string }>;
				count: number;
			};
			const installedRow = s.installed.some((i) => i.id === PKG_ID);
			const sidecarRegistered = sidecars.entries.some(
				(e) => e.pkg_id === PKG_ID && e.name === 'pa-com-example-demo-main'
			);
			return { installedRow, sidecarRegistered, apiVersion: s.api_version };
		};

		(async () => {
			log('PHASE', phase);

			if (phase === 'verify') {
				try {
					const r = await checkStatus();
					const ok = r.installedRow && r.sidecarRegistered;
					log('STATUS', `installed=${r.installedRow} sidecar_registered=${r.sidecarRegistered}`);
					if (!cancelled) setVerdict(ok ? 'VERDICT PASS' : 'VERDICT FAIL');
				} catch (e) {
					log('STATUS', `FAIL ${(e as Error).message ?? String(e)}`);
					if (!cancelled) setVerdict('VERDICT FAIL');
				}
				return;
			}

			if (phase === 'cleanup') {
				try {
					await pkgUninstall(PKG_ID);
					log('UNINSTALL', 'OK');
					if (!cancelled) setVerdict('VERDICT PASS');
				} catch (e) {
					log('UNINSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
					if (!cancelled) setVerdict('VERDICT FAIL');
				}
				return;
			}

			// install + (optionally) roundtrip
			try {
				await pkgUninstall(PKG_ID);
			} catch {
				// not installed — fine
			}

			let installed = false;
			try {
				const r = await pkgInstallFromPath(PKG_PATH);
				installed = true;
				log('INSTALL', `OK id=${r.installed.id} version=${r.installed.version}`);
			} catch (e) {
				log('INSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			let statusOk = false;
			let sidecarRegistered = false;
			try {
				const r = await checkStatus();
				statusOk = r.installedRow;
				sidecarRegistered = r.sidecarRegistered;
				log(
					'STATUS',
					`installed=${r.installedRow} sidecar_registered=${r.sidecarRegistered} api_version=${r.apiVersion}`
				);
			} catch (e) {
				log('STATUS', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			if (phase === 'install') {
				const ok = installed && statusOk && sidecarRegistered;
				if (!cancelled)
					setVerdict(ok ? 'VERDICT PASS (left installed for boot replay)' : 'VERDICT FAIL');
				log('DONE', ok ? 'PASS-leave-installed' : 'FAIL');
				return;
			}

			let uninstallOk = false;
			try {
				await pkgUninstall(PKG_ID);
				const r = await checkStatus();
				uninstallOk = !r.installedRow && !r.sidecarRegistered;
				log('UNINSTALL', `OK pkg_gone=${!r.installedRow} sidecar_gone=${!r.sidecarRegistered}`);
			} catch (e) {
				log('UNINSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			const ok = installed && statusOk && sidecarRegistered && uninstallOk;
			const v = ok ? 'VERDICT PASS' : 'VERDICT FAIL';
			log('DONE', v);
			if (!cancelled) setVerdict(v);
		})();

		return () => {
			cancelled = true;
		};
	}, [phase]);

	return (
		<div style={{ padding: 24, fontFamily: 'monospace', fontSize: 13 }}>
			<h1 style={{ fontSize: 16, marginBottom: 12 }}>Smoke: Pkg Kernel</h1>
			<div data-testid="pkg-smoke-verdict" style={{ marginBottom: 16, fontWeight: 'bold' }}>
				{verdict}
			</div>
			<ul style={{ listStyle: 'none', padding: 0 }}>
				{rows.map((r, i) => (
					<li key={i} data-testid={`pkg-smoke-row-${r.label}`}>
						<strong>{r.label}</strong> — {r.outcome}
					</li>
				))}
			</ul>
		</div>
	);
}
