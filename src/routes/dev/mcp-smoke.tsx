// MCP registry smoke test.
//
// 1. Install /tmp/test-pkg-com.example.mcp (declares 2 stdio MCP servers).
// 2. Status → assert registry surfaces both entries with `pkg-com-example-mcp-*` keys.
// 3. Uninstall → assert registry empty for the pkg.
//
// Filesystem-side verification (the entries land in ~/.claude.json:mcpServers
// and are removed on uninstall) happens out-of-band via shell — the
// allowlisted fs commands don't reach the home-level .claude.json.
import { createFileRoute, notFound } from '@tanstack/react-router';
import { useEffect, useState } from 'react';

import { pkgInstallFromPath, pkgKernelStatus, pkgUninstall } from '@/lib/tauri-cmd';

export const Route = createFileRoute('/dev/mcp-smoke')({
	beforeLoad: () => {
		if (!import.meta.env.DEV) {
			throw notFound();
		}
	},
	component: import.meta.env.DEV ? McpSmoke : () => null,
});

const PKG_PATH = '/tmp/test-pkg-com.example.mcp';
const PKG_ID = 'com.example.mcp';
const PKG_SLUG = 'com-example-mcp';

type Row = { label: string; outcome: string };

interface Entry {
	pkg_id: string;
	name: string;
	key: string;
}

function McpSmoke() {
	const [rows, setRows] = useState<Row[]>([]);
	const [verdict, setVerdict] = useState('RUNNING');

	useEffect(() => {
		let cancelled = false;
		const log = (label: string, outcome: string) => {
			if (cancelled) return;
			// eslint-disable-next-line no-console
			console.log(`[mcp-smoke] ${label}: ${outcome}`);
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
				const reg = s.registries.mcp as
					| { entries: Entry[]; count: number; config_path?: string }
					| undefined;
				const mine = (reg?.entries ?? []).filter((e) => e.pkg_id === PKG_ID);
				const echo = mine.find((e) => e.name === 'echo-server');
				const ls = mine.find((e) => e.name === 'ls-server');
				registryOk =
					mine.length === 2 &&
					echo?.key === `pkg-${PKG_SLUG}-echo-server` &&
					ls?.key === `pkg-${PKG_SLUG}-ls-server`;
				log(
					'REGISTRY',
					`pkg_mcps=${mine.length} keys=${JSON.stringify(mine.map((m) => m.key))} config=${reg?.config_path ?? 'n/a'}`
				);
			} catch (e) {
				log('REGISTRY', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			let uninstallOk = false;
			try {
				await pkgUninstall(PKG_ID);
				const s = await pkgKernelStatus();
				const reg = s.registries.mcp as { entries: Entry[]; count: number } | undefined;
				const mine = (reg?.entries ?? []).filter((e) => e.pkg_id === PKG_ID);
				uninstallOk = mine.length === 0;
				log('UNINSTALL', `pkg_mcps_after=${mine.length}`);
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
			<h1 style={{ fontSize: 16, marginBottom: 12 }}>Smoke: MCP Registry</h1>
			<div data-testid="mcp-smoke-verdict" style={{ marginBottom: 16, fontWeight: 'bold' }}>
				{verdict}
			</div>
			<ul style={{ listStyle: 'none', padding: 0 }}>
				{rows.map((r, i) => (
					<li key={i} data-testid={`mcp-smoke-row-${i}`}>
						<strong>{r.label}</strong> — {r.outcome}
					</li>
				))}
			</ul>
		</div>
	);
}
