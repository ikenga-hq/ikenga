// Settings registry smoke test.
//
// 1. Install /tmp/test-pkg-com.example.settings (declares 3 fields with defaults).
// 2. status → assert registry surfaces the schema for the pkg.
// 3. pkgSettingsGet → assert seeded defaults are present.
// 4. pkgSettingsSet → override one field, re-read, assert new value sticks.
// 5. Uninstall → assert schema gone from snapshot.
import { createFileRoute, notFound } from '@tanstack/react-router';
import { useEffect, useState } from 'react';

import {
	pkgInstallFromPath,
	pkgKernelStatus,
	pkgSettingsGet,
	pkgSettingsSet,
	pkgUninstall,
} from '@/lib/tauri-cmd';

export const Route = createFileRoute('/dev/settings-smoke')({
	beforeLoad: () => {
		if (!import.meta.env.DEV) {
			throw notFound();
		}
	},
	component: import.meta.env.DEV ? SettingsSmoke : () => null,
});

const PKG_PATH = '/tmp/test-pkg-com.example.settings';
const PKG_ID = 'com.example.settings';

type Row = { label: string; outcome: string };

function SettingsSmoke() {
	const [rows, setRows] = useState<Row[]>([]);
	const [verdict, setVerdict] = useState('RUNNING');

	useEffect(() => {
		let cancelled = false;
		const log = (label: string, outcome: string) => {
			if (cancelled) return;
			// eslint-disable-next-line no-console
			console.log(`[settings-smoke] ${label}: ${outcome}`);
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

			let schemaOk = false;
			try {
				const s = await pkgKernelStatus();
				const reg = s.registries.settings as
					| {
							entries: Array<{ pkg_id: string; schema: unknown[]; values: unknown }>;
							count: number;
					  }
					| undefined;
				const mine = (reg?.entries ?? []).find((e) => e.pkg_id === PKG_ID);
				schemaOk = !!mine && Array.isArray(mine.schema) && mine.schema.length === 3;
				log(
					'REGISTRY',
					`count=${reg?.count ?? 'n/a'} pkg_present=${!!mine} schema_len=${mine?.schema?.length ?? 0}`
				);
			} catch (e) {
				log('REGISTRY', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			let defaultsOk = false;
			try {
				const r = await pkgSettingsGet(PKG_ID);
				const v = r.values;
				defaultsOk = v.api_token === '' && v.max_items === 50 && v.enabled === true;
				log(
					'DEFAULTS',
					`api_token=${JSON.stringify(v.api_token)} max_items=${JSON.stringify(v.max_items)} enabled=${JSON.stringify(v.enabled)} match=${defaultsOk}`
				);
			} catch (e) {
				log('DEFAULTS', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			let setOk = false;
			try {
				await pkgSettingsSet(PKG_ID, 'max_items', 99);
				const r = await pkgSettingsGet(PKG_ID);
				setOk = r.values.max_items === 99;
				log('SET', `max_items=${JSON.stringify(r.values.max_items)} match=${setOk}`);
			} catch (e) {
				log('SET', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			let uninstallOk = false;
			try {
				await pkgUninstall(PKG_ID);
				const s = await pkgKernelStatus();
				const reg = s.registries.settings as
					| { entries: Array<{ pkg_id: string }>; count: number }
					| undefined;
				const stillThere = (reg?.entries ?? []).some((e) => e.pkg_id === PKG_ID);
				uninstallOk = !stillThere;
				log('UNINSTALL', `OK schema_gone=${!stillThere}`);
			} catch (e) {
				log('UNINSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			const ok = installed && schemaOk && defaultsOk && setOk && uninstallOk;
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
			<h1 style={{ fontSize: 16, marginBottom: 12 }}>Smoke: Settings Registry</h1>
			<div data-testid="settings-smoke-verdict" style={{ marginBottom: 16, fontWeight: 'bold' }}>
				{verdict}
			</div>
			<ul style={{ listStyle: 'none', padding: 0 }}>
				{rows.map((r, i) => (
					<li key={i} data-testid={`settings-smoke-row-${i}`}>
						<strong>{r.label}</strong> — {r.outcome}
					</li>
				))}
			</ul>
		</div>
	);
}
