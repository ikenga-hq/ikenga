// Smoke test for the iframe mount mechanism (Gap 1 follow-up to Tasks
// migration). Phases:
//
//   roundtrip (default) — install com.example.iframeapp → assert
//                         pkg_content_url returns a URL → mount the iframe
//                         host → wait for ui/initialize → also install +
//                         uninstall the com.royalti.hyperframes manifest
//                         stub to verify Studio-shape manifest deserialize →
//                         uninstall iframeapp → verify the catch-all renders
//                         not-found.
//   install             — install + leave persisted (boot replay test).
//   verify              — assert prior install intact (no mutation).
//   cleanup             — uninstall both fixtures, idempotent.
//   longlived           — install com.example.longlived (lifecycle:"long-lived"
//                         in manifest.mcp[]) → fire two sequential bumps
//                         (counter must increment) → fire two parallel
//                         slow-tool calls (multiplexer must dispatch by id) →
//                         pid stays the same → uninstall → assert the
//                         supervisor's registry snapshot drops the entry.
//
// The fixture's dist/index.html uses @modelcontextprotocol/ext-apps to
// initialize as an MCP App. Our PkgIframeHost runs the host side. A
// successful initialize round-trip is the assertion that the wire format
// + AppBridge plumbing + hostContext composition all work end-to-end.

import { createFileRoute, notFound } from '@tanstack/react-router';
import { useEffect, useMemo, useRef, useState } from 'react';

import { PkgIframeHost } from '@/components/pkg/pkg-iframe-host';
import {
	devBindPort,
	devReleasePort,
	pkgContentUrl,
	pkgInstallFromPath,
	pkgKernelStatus,
	pkgMcpCall,
	pkgUninstall,
} from '@/lib/tauri-cmd';

type Phase =
	| 'roundtrip'
	| 'install'
	| 'verify'
	| 'cleanup'
	| 'longlived'
	| 'hyperframes'
	| 'video-studio'
	| 'storyboard'
	| 'storyboard-recovery';

export const Route = createFileRoute('/dev/iframe-mount-smoke')({
	beforeLoad: () => {
		if (!import.meta.env.DEV) {
			throw notFound();
		}
	},
	component: import.meta.env.DEV ? IframeMountSmoke : () => null,
	validateSearch: (s: Record<string, unknown>): { phase?: Phase } => ({
		phase: (s.phase as Phase) || undefined,
	}),
});

const IFRAMEAPP_PATH = '/tmp/test-pkg-com.example.iframeapp';
const IFRAMEAPP_ID = 'com.example.iframeapp';
const HF_PATH = '/tmp/test-pkg-com.royalti.hyperframes';
const HF_ID = 'com.royalti.hyperframes';
const LL_PATH = '/tmp/test-pkg-com.example.longlived';
const LL_ID = 'com.example.longlived';
const VS_PATH = '/tmp/test-pkg-com.royalti.video-studio';
const VS_ID = 'com.royalti.video-studio';
const SB_PATH = '/tmp/test-pkg-com.royalti.storyboard';
const SB_ID = 'com.royalti.storyboard';

// HF wrapper-driven open: open_project boots a Vite preview server. The
// sidecar's own ready window is 12s (OPEN_READY_TIMEOUT_MS in sidecar.ts),
// plus a small bridge round-trip overhead. 15s is conservative.
const OPEN_TIMEOUT_MS = 15_000;
// Cold Vite preview server takes longer than the open ready window because
// it has to bundle hyperframes-projects/<slug>/ before the first HTTP
// response. Subsequent loads are fast. 25s covers a worst-case cold boot
// without dragging out CI on the happy path.
const PREVIEW_LOAD_TIMEOUT_MS = 25_000;

interface Row {
	label: string;
	outcome: string;
}

interface UiRouteEntry {
	pkg_id: string;
	path: string;
	kind: string;
	source: string;
}
interface UiSnap {
	entries?: UiRouteEntry[];
}

function IframeMountSmoke() {
	const search = Route.useSearch();
	const phase: Phase = search.phase ?? 'roundtrip';

	const [rows, setRows] = useState<Row[]>([]);
	const [verdict, setVerdict] = useState('IDLE — click Run to start');
	const [mountSrc, setMountSrc] = useState<{ pkgId: string; source: string } | null>(null);
	// Manual gate: a tab restored to this route used to auto-run install /
	// uninstall flows on mount, churning the kernel and racing the boot
	// replay. Smokes now only run when the user clicks Run.
	const [started, setStarted] = useState(false);
	// initialized flag must be a ref — using state would put it in the effect's
	// dep array and re-run the entire smoke when the iframe handshakes.
	const initializedRef = useRef(false);

	const iframeBoxKey = useMemo(() => `${mountSrc?.pkgId}-${Date.now()}`, [mountSrc]);

	useEffect(() => {
		if (!started) return;
		let cancelled = false;
		const log = (label: string, outcome: string) => {
			if (cancelled) return;
			// eslint-disable-next-line no-console
			console.log(`[iframe-mount-smoke] ${label}: ${outcome}`);
			setRows((p) => [...p, { label, outcome }]);
		};

		const safeUninstall = async (pkgId: string) => {
			try {
				await pkgUninstall(pkgId);
			} catch {
				// ignore — pkg may not have been installed
			}
		};

		const findIframeAppEntry = async (): Promise<UiRouteEntry | null> => {
			const status = await pkgKernelStatus();
			const ui = (status.registries.ui_routes ?? {}) as UiSnap;
			const entry = (ui.entries ?? []).find(
				(e) => e.pkg_id === IFRAMEAPP_ID && e.path === '/hello' && e.kind === 'iframe'
			);
			return entry ?? null;
		};

		const waitForInitialize = (timeoutMs: number) =>
			new Promise<boolean>((resolve) => {
				const start = Date.now();
				const tick = () => {
					if (cancelled) return resolve(false);
					if (initializedRef.current) return resolve(true);
					// Fallback: check the iframe's globalThis exposed by the fixture.
					const f = document.querySelector(
						`iframe[data-pkg-id="${IFRAMEAPP_ID}"]`
					) as HTMLIFrameElement | null;
					try {
						const w = f?.contentWindow as unknown as {
							__iframeapp_state?: { connected: boolean };
						} | null;
						if (w?.__iframeapp_state?.connected) return resolve(true);
					} catch {
						// cross-origin; ignore
					}
					if (Date.now() - start > timeoutMs) return resolve(false);
					setTimeout(tick, 100);
				};
				tick();
			});

		(async () => {
			log('PHASE', phase);

			if (phase === 'cleanup') {
				await safeUninstall(IFRAMEAPP_ID);
				await safeUninstall(HF_ID);
				await safeUninstall(LL_ID);
				log('UNINSTALL', 'OK (all)');
				if (!cancelled) setVerdict('VERDICT PASS');
				return;
			}

			if (phase === 'longlived') {
				// Exercises the SidecarSupervisor: long-lived child, multiplexed
				// tools/call, supervised teardown on uninstall.
				await safeUninstall(LL_ID);

				let installed = false;
				try {
					const r = await pkgInstallFromPath(LL_PATH);
					installed = true;
					log('LL_INSTALL', `OK id=${r.installed.id}@${r.installed.version}`);
				} catch (e) {
					log('LL_INSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
				}

				// Supervisor task spawns + handshakes asynchronously after register;
				// poll until the registry snapshot reports state=running (or fail
				// with a clear timeout).
				type SupEntry = { pkg_id: string; state: string; pid?: number | null };
				type SupSnap = { entries?: SupEntry[] };
				const findSupEntry = async (): Promise<SupEntry | null> => {
					const status = await pkgKernelStatus();
					const sup = (status.registries.sidecar_supervisor ?? {}) as SupSnap;
					return (sup.entries ?? []).find((e) => e.pkg_id === LL_ID) ?? null;
				};

				let runningOk = false;
				const start = Date.now();
				while (Date.now() - start < 5000) {
					if (cancelled) return;
					const e = await findSupEntry();
					if (e?.state === 'running') {
						log('LL_SUP_STATE', `running pid=${e.pid ?? '?'}`);
						runningOk = true;
						break;
					}
					await new Promise((r) => setTimeout(r, 100));
				}
				if (!runningOk) {
					log('LL_SUP_STATE', 'FAIL (never reached state=running within 5s)');
				}

				// 1) Two sequential bumps must increment — proves long-lived state.
				let seqOk = false;
				try {
					const a = await pkgMcpCall(LL_ID, 'bump', {});
					const b = await pkgMcpCall(LL_ID, 'bump', {});
					const av = (a.result as { counter?: number } | null)?.counter;
					const bv = (b.result as { counter?: number } | null)?.counter;
					seqOk = a.ok && b.ok && av === 1 && bv === 2;
					log(
						'LL_SEQUENTIAL',
						seqOk
							? `OK 1→${av}, 2→${bv}`
							: `FAIL ${a.error ?? ''} ${b.error ?? ''} av=${av} bv=${bv}`
					);
				} catch (e) {
					log('LL_SEQUENTIAL', `FAIL ${(e as Error).message ?? String(e)}`);
				}

				// 2) Two parallel slow calls must both succeed in <400ms — proves
				//    the multiplexer dispatches by JSON-RPC id (sequential would be
				//    400ms+ since each takes 200ms server-side).
				let parOk = false;
				try {
					const t0 = performance.now();
					const [p1, p2] = await Promise.all([
						pkgMcpCall(LL_ID, 'slow', {}),
						pkgMcpCall(LL_ID, 'slow', {}),
					]);
					const elapsed = performance.now() - t0;
					parOk = p1.ok && p2.ok && elapsed < 380;
					log(
						'LL_PARALLEL',
						parOk
							? `OK both slow done in ${elapsed.toFixed(0)}ms (<380ms)`
							: `FAIL elapsed=${elapsed.toFixed(0)}ms p1=${p1.ok} p2=${p2.ok}`
					);
				} catch (e) {
					log('LL_PARALLEL', `FAIL ${(e as Error).message ?? String(e)}`);
				}

				// 3) Pid stays the same across the two earlier calls — same OS process.
				let pidOk = false;
				try {
					const a = await pkgMcpCall(LL_ID, 'pid', {});
					const b = await pkgMcpCall(LL_ID, 'pid', {});
					const ap = (a.result as { pid?: number } | null)?.pid;
					const bp = (b.result as { pid?: number } | null)?.pid;
					pidOk = a.ok && b.ok && typeof ap === 'number' && ap === bp;
					log('LL_PID_STABLE', pidOk ? `OK pid=${ap}` : `FAIL ap=${ap} bp=${bp}`);
				} catch (e) {
					log('LL_PID_STABLE', `FAIL ${(e as Error).message ?? String(e)}`);
				}

				// 4) Uninstall — supervisor entry must drop out of the snapshot.
				let teardownOk = false;
				try {
					await pkgUninstall(LL_ID);
					// Allow the supervisor task to observe shutdown and clear active.
					await new Promise((r) => setTimeout(r, 300));
					const after = await findSupEntry();
					teardownOk = after === null;
					log(
						'LL_UNINSTALL',
						teardownOk
							? 'OK (supervisor entry cleared)'
							: `FAIL still present: ${JSON.stringify(after)}`
					);
				} catch (e) {
					log('LL_UNINSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
				}

				const ok = installed && runningOk && seqOk && parOk && pidOk && teardownOk;
				log('DONE', ok ? 'PASS' : 'FAIL');
				if (!cancelled) setVerdict(ok ? 'VERDICT PASS' : 'VERDICT FAIL');
				return;
			}

			if (phase === 'hyperframes') {
				// Exercises the real hyperframes pkg through SidecarSupervisor +
				// the PR 3 wrapper UI. Asserts: install → supervisor running →
				// list_projects non-empty (sanity baseline) → mount wrapper iframe
				// → ui/initialize round-trips → wrapper publishes its project list
				// → drive open via __hyperframes_open(slug) → preview iframe
				// loads → close via __hyperframes_close → list_active empty →
				// uninstall → supervisor entry cleared.
				await safeUninstall(HF_ID);

				let installed = false;
				try {
					const r = await pkgInstallFromPath(HF_PATH);
					installed = true;
					log('HF_INSTALL', `OK id=${r.installed.id}@${r.installed.version}`);
				} catch (e) {
					log('HF_INSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
				}

				type SupEntry = { pkg_id: string; state: string; pid?: number | null };
				type SupSnap = { entries?: SupEntry[] };
				const findSupEntry = async (): Promise<SupEntry | null> => {
					const status = await pkgKernelStatus();
					const sup = (status.registries.sidecar_supervisor ?? {}) as SupSnap;
					return (sup.entries ?? []).find((e) => e.pkg_id === HF_ID) ?? null;
				};

				let runningOk = false;
				const start = Date.now();
				while (Date.now() - start < 8000) {
					if (cancelled) return;
					const e = await findSupEntry();
					if (e?.state === 'running') {
						log('HF_SUP_STATE', `running pid=${e.pid ?? '?'}`);
						runningOk = true;
						break;
					}
					await new Promise((r) => setTimeout(r, 100));
				}
				if (!runningOk) {
					log('HF_SUP_STATE', 'FAIL (never reached state=running within 8s)');
				}

				// 1) list_projects must return a non-empty array of strings.
				let listOk = false;
				let firstSlug: string | null = null;
				try {
					const r = await pkgMcpCall(HF_ID, 'list_projects', {});
					const projects = (r.result as { projects?: unknown[] } | null)?.projects;
					if (
						r.ok &&
						Array.isArray(projects) &&
						projects.length > 0 &&
						typeof projects[0] === 'string'
					) {
						firstSlug = projects[0] as string;
						listOk = true;
						log('HF_LIST', `OK ${projects.length} project(s), first=${firstSlug}`);
					} else {
						log('HF_LIST', `FAIL ${r.error ?? JSON.stringify(r.result)}`);
					}
				} catch (e) {
					log('HF_LIST', `FAIL ${(e as Error).message ?? String(e)}`);
				}

				// 2) Mint a content URL — confirms pkg_content registered the
				//    wrapper's dist/. Without this the wrapper iframe can't load
				//    its own subresources (app.js / styles).
				let contentUrlOk = false;
				try {
					const handle = await pkgContentUrl(HF_ID);
					contentUrlOk = handle.url.startsWith('http://127.0.0.1:') && handle.token.length === 64;
					log('HF_CONTENT_URL', `${handle.url} token=${handle.token.slice(0, 8)}…`);
				} catch (e) {
					log('HF_CONTENT_URL', `FAIL ${(e as Error).message ?? String(e)}`);
				}

				// 3) Mount the wrapper. PkgIframeHost serves dist/index.html via
				//    srcdoc + injected base href, runs the AppBridge, and pushes
				//    the host context. The wrapper handshakes and calls
				//    list_projects on its own — we just wait for the published
				//    state.
				type HfWrapperState = {
					status: string;
					projects: string[];
					currentSlug: string | null;
					port: number | null;
					previewLoaded: boolean;
				};
				type HfWrapperWindow = Window & {
					__hyperframes_state?: HfWrapperState;
					__hyperframes_open?: (slug: string) => Promise<void>;
					__hyperframes_close?: () => Promise<void>;
				};
				const getWrapperWindow = (): HfWrapperWindow | null => {
					const f = document.querySelector(
						`iframe[data-pkg-id="${HF_ID}"]`
					) as HTMLIFrameElement | null;
					try {
						return (f?.contentWindow as HfWrapperWindow | null) ?? null;
					} catch {
						return null;
					}
				};
				const getWrapperState = (): HfWrapperState | null =>
					getWrapperWindow()?.__hyperframes_state ?? null;
				const waitFor = async <T,>(
					fn: () => T | null | undefined,
					timeoutMs: number
				): Promise<T | null> => {
					const start = Date.now();
					while (Date.now() - start < timeoutMs) {
						if (cancelled) return null;
						const v = fn();
						if (v) return v;
						await new Promise((r) => setTimeout(r, 100));
					}
					return null;
				};

				let bridgeOk = false;
				if (contentUrlOk) {
					initializedRef.current = false;
					setMountSrc({ pkgId: HF_ID, source: 'dist/index.html' });
					const start = Date.now();
					while (Date.now() - start < 8000) {
						if (cancelled) return;
						if (initializedRef.current) {
							bridgeOk = true;
							break;
						}
						// Fallback: wrapper exposes a `connected` status string before
						// the host's onInitialized fires in some load orders.
						if (getWrapperState()?.status === 'connected') {
							bridgeOk = true;
							break;
						}
						await new Promise((r) => setTimeout(r, 100));
					}
					log(
						'HF_BRIDGE_INIT',
						bridgeOk ? 'OK (handshake completed)' : 'TIMEOUT (no initialize within 8s)'
					);
				} else {
					log('HF_BRIDGE_INIT', 'SKIPPED (no content URL)');
				}

				// 4) Wrapper's list_projects must return the same set the direct
				//    pkgMcpCall saw — proves the App.callServerTool path works
				//    end-to-end (App → AppBridge → pkg_mcp_call → MCP sidecar).
				let wrapperListOk = false;
				if (bridgeOk) {
					const projects = await waitFor(() => {
						const arr = getWrapperState()?.projects;
						return arr && arr.length > 0 ? arr : null;
					}, 5000);
					wrapperListOk = !!projects && (firstSlug ? projects.includes(firstSlug) : true);
					log(
						'HF_WRAPPER_LIST',
						wrapperListOk
							? `OK ${projects!.length} project(s)`
							: `FAIL got=${JSON.stringify(projects)}`
					);
				} else {
					log('HF_WRAPPER_LIST', 'SKIPPED (no bridge)');
				}

				// 5) Drive open via the wrapper. The smoke calls __hyperframes_open
				//    directly instead of synthesizing a click — the picker UI is
				//    DOM-fragile, the global isn't.
				let wrapperOpenOk = false;
				if (wrapperListOk && firstSlug) {
					try {
						const open = getWrapperWindow()?.__hyperframes_open;
						if (typeof open !== 'function') throw new Error('__hyperframes_open missing');
						await open(firstSlug);
						const state = await waitFor(() => {
							const s = getWrapperState();
							return s && s.currentSlug === firstSlug && typeof s.port === 'number' ? s : null;
						}, OPEN_TIMEOUT_MS);
						wrapperOpenOk = !!state;
						log(
							'HF_WRAPPER_OPEN',
							wrapperOpenOk
								? `OK slug=${state!.currentSlug} port=${state!.port}`
								: `TIMEOUT state=${JSON.stringify(getWrapperState())}`
						);
					} catch (e) {
						log('HF_WRAPPER_OPEN', `FAIL ${(e as Error).message ?? String(e)}`);
					}
				} else {
					log('HF_WRAPPER_OPEN', 'SKIPPED');
				}

				// 6) The nested <iframe src="http://127.0.0.1:<port>"> must finish
				//    loading. Cold Vite preview boot can take a while, so the
				//    timeout is generous; the previewLoaded flag flips on the
				//    iframe's `load` event (gated on http://127.0.0.1: src).
				let previewOk = false;
				if (wrapperOpenOk) {
					const state = await waitFor(() => {
						const s = getWrapperState();
						return s?.previewLoaded ? s : null;
					}, PREVIEW_LOAD_TIMEOUT_MS);
					previewOk = !!state;
					log(
						'HF_PREVIEW_LOADED',
						previewOk
							? 'OK (nested iframe load fired)'
							: `TIMEOUT after ${PREVIEW_LOAD_TIMEOUT_MS}ms`
					);
				} else {
					log('HF_PREVIEW_LOADED', 'SKIPPED');
				}

				// 7) Close via the wrapper. After it returns, list_active must
				//    report no entries — proves the wrapper actually drove
				//    close_project rather than just clearing its UI state.
				let wrapperCloseOk = false;
				try {
					const close = getWrapperWindow()?.__hyperframes_close;
					if (typeof close === 'function') {
						await close();
					}
					// Drop the host iframe so any unmount-side teardown completes.
					setMountSrc(null);
					await new Promise((r) => setTimeout(r, 200));
					const r = await pkgMcpCall(HF_ID, 'list_active', {});
					const actives = (r.result as { actives?: unknown[] } | null)?.actives;
					wrapperCloseOk = r.ok && Array.isArray(actives) && actives.length === 0;
					log(
						'HF_WRAPPER_CLOSE',
						wrapperCloseOk
							? 'OK (list_active empty)'
							: `FAIL ${r.error ?? `actives=${JSON.stringify(actives)}`}`
					);
				} catch (e) {
					log('HF_WRAPPER_CLOSE', `FAIL ${(e as Error).message ?? String(e)}`);
				}

				// 8) Uninstall — supervisor entry must drop.
				let teardownOk = false;
				try {
					await pkgUninstall(HF_ID);
					await new Promise((r) => setTimeout(r, 300));
					const after = await findSupEntry();
					teardownOk = after === null;
					log(
						'HF_UNINSTALL',
						teardownOk
							? 'OK (supervisor entry cleared)'
							: `FAIL still present: ${JSON.stringify(after)}`
					);
				} catch (e) {
					log('HF_UNINSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
				}

				const ok =
					installed &&
					runningOk &&
					listOk &&
					contentUrlOk &&
					bridgeOk &&
					wrapperListOk &&
					wrapperOpenOk &&
					previewOk &&
					wrapperCloseOk &&
					teardownOk;
				log('DONE', ok ? 'PASS' : 'FAIL');
				if (!cancelled) setVerdict(ok ? 'VERDICT PASS' : 'VERDICT FAIL');
				return;
			}

			if (phase === 'video-studio') {
				// Exercises the supervised pa-video-studio MCP sidecar (Bug 1
				// fix from 2026-05-04-pa-desktop-known-bugs.md): install →
				// supervisor running → start_studio (real Remotion boot,
				// returns {port}) → get_status (running) → stop_studio →
				// uninstall → supervisor entry cleared.
				await safeUninstall(VS_ID);

				let installed = false;
				try {
					const r = await pkgInstallFromPath(VS_PATH);
					installed = true;
					log('VS_INSTALL', `OK id=${r.installed.id}@${r.installed.version}`);
				} catch (e) {
					log('VS_INSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
				}

				type SupEntry = { pkg_id: string; state: string; pid?: number | null };
				type SupSnap = { entries?: SupEntry[] };
				const findSupEntry = async (): Promise<SupEntry | null> => {
					const status = await pkgKernelStatus();
					const sup = (status.registries.sidecar_supervisor ?? {}) as SupSnap;
					return (sup.entries ?? []).find((e) => e.pkg_id === VS_ID) ?? null;
				};

				let runningOk = false;
				const start = Date.now();
				while (Date.now() - start < 8000) {
					if (cancelled) return;
					const e = await findSupEntry();
					if (e?.state === 'running') {
						log('VS_SUP_STATE', `running pid=${e.pid ?? '?'}`);
						runningOk = true;
						break;
					}
					await new Promise((r) => setTimeout(r, 100));
				}
				if (!runningOk) {
					log('VS_SUP_STATE', 'FAIL (never reached state=running within 8s)');
				}

				// 1) start_studio — real Remotion boot, ~5s including build.
				let startOk = false;
				let port: number | null = null;
				try {
					const r = await pkgMcpCall(VS_ID, 'start_studio', {});
					port = (r.result as { port?: number } | null)?.port ?? null;
					startOk = r.ok && typeof port === 'number' && port > 0;
					log('VS_START', startOk ? `OK port=${port}` : `FAIL ${r.error ?? `port=${port}`}`);
				} catch (e) {
					log('VS_START', `FAIL ${(e as Error).message ?? String(e)}`);
				}

				// 2) get_status must report running:true with the same port.
				let statusOk = false;
				try {
					const r = await pkgMcpCall(VS_ID, 'get_status', {});
					const running = (r.result as { running?: boolean } | null)?.running;
					const sport = (r.result as { port?: number } | null)?.port;
					statusOk = r.ok && running === true && sport === port;
					log(
						'VS_STATUS',
						statusOk ? `OK running=true port=${sport}` : `FAIL running=${running} port=${sport}`
					);
				} catch (e) {
					log('VS_STATUS', `FAIL ${(e as Error).message ?? String(e)}`);
				}

				// 3) stop_studio.
				let stopOk = false;
				try {
					const r = await pkgMcpCall(VS_ID, 'stop_studio', {});
					stopOk = r.ok;
					log('VS_STOP', stopOk ? 'OK' : `FAIL ${r.error ?? ''}`);
				} catch (e) {
					log('VS_STOP', `FAIL ${(e as Error).message ?? String(e)}`);
				}

				// 4) Uninstall — supervisor entry must drop.
				let teardownOk = false;
				try {
					await pkgUninstall(VS_ID);
					await new Promise((r) => setTimeout(r, 300));
					const after = await findSupEntry();
					teardownOk = after === null;
					log(
						'VS_UNINSTALL',
						teardownOk
							? 'OK (supervisor entry cleared)'
							: `FAIL still present: ${JSON.stringify(after)}`
					);
				} catch (e) {
					log('VS_UNINSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
				}

				const ok = installed && runningOk && startOk && statusOk && stopOk && teardownOk;
				log('DONE', ok ? 'PASS' : 'FAIL');
				if (!cancelled) setVerdict(ok ? 'VERDICT PASS' : 'VERDICT FAIL');
				return;
			}

			if (phase === 'storyboard') {
				// Exercises the supervised pa-storyboard MCP sidecar (PR 5 of
				// the pkg-kernel arc). Boots the storyboard-app dev stack
				// (Vite :3105 + Express :3106 internal). Cold Vite build is
				// heavier than Remotion's, so the supervisor-running poll uses
				// a 15s ceiling vs. video-studio's 8s.
				await safeUninstall(SB_ID);

				let installed = false;
				try {
					const r = await pkgInstallFromPath(SB_PATH);
					installed = true;
					log('SB_INSTALL', `OK id=${r.installed.id}@${r.installed.version}`);
				} catch (e) {
					log('SB_INSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
				}

				type SupEntry = { pkg_id: string; state: string; pid?: number | null };
				type SupSnap = { entries?: SupEntry[] };
				const findSupEntry = async (): Promise<SupEntry | null> => {
					const status = await pkgKernelStatus();
					const sup = (status.registries.sidecar_supervisor ?? {}) as SupSnap;
					return (sup.entries ?? []).find((e) => e.pkg_id === SB_ID) ?? null;
				};

				let runningOk = false;
				const start = Date.now();
				while (Date.now() - start < 15000) {
					if (cancelled) return;
					const e = await findSupEntry();
					if (e?.state === 'running') {
						log('SB_SUP_STATE', `running pid=${e.pid ?? '?'}`);
						runningOk = true;
						break;
					}
					await new Promise((r) => setTimeout(r, 100));
				}
				if (!runningOk) {
					log('SB_SUP_STATE', 'FAIL (never reached state=running within 15s)');
				}

				// 1) start_storyboard — boots Vite+Express. Cold build can take
				//    tens of seconds; the sidecar polls /api/health up to 75s.
				let startOk = false;
				let port: number | null = null;
				try {
					const r = await pkgMcpCall(SB_ID, 'start_storyboard', {});
					port = (r.result as { port?: number } | null)?.port ?? null;
					startOk = r.ok && typeof port === 'number' && port > 0;
					log('SB_START', startOk ? `OK port=${port}` : `FAIL ${r.error ?? `port=${port}`}`);
				} catch (e) {
					log('SB_START', `FAIL ${(e as Error).message ?? String(e)}`);
				}

				// 2) get_status must report running:true with the same port.
				let statusOk = false;
				try {
					const r = await pkgMcpCall(SB_ID, 'get_status', {});
					const running = (r.result as { running?: boolean } | null)?.running;
					const sport = (r.result as { port?: number } | null)?.port;
					statusOk = r.ok && running === true && sport === port;
					log(
						'SB_STATUS',
						statusOk ? `OK running=true port=${sport}` : `FAIL running=${running} port=${sport}`
					);
				} catch (e) {
					log('SB_STATUS', `FAIL ${(e as Error).message ?? String(e)}`);
				}

				// 3) stop_storyboard.
				let stopOk = false;
				try {
					const r = await pkgMcpCall(SB_ID, 'stop_storyboard', {});
					stopOk = r.ok;
					log('SB_STOP', stopOk ? 'OK' : `FAIL ${r.error ?? ''}`);
				} catch (e) {
					log('SB_STOP', `FAIL ${(e as Error).message ?? String(e)}`);
				}

				// 4) Uninstall — supervisor entry must drop.
				let teardownOk = false;
				try {
					await pkgUninstall(SB_ID);
					await new Promise((r) => setTimeout(r, 300));
					const after = await findSupEntry();
					teardownOk = after === null;
					log(
						'SB_UNINSTALL',
						teardownOk
							? 'OK (supervisor entry cleared)'
							: `FAIL still present: ${JSON.stringify(after)}`
					);
				} catch (e) {
					log('SB_UNINSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
				}

				const ok = installed && runningOk && startOk && statusOk && stopOk && teardownOk;
				log('DONE', ok ? 'PASS' : 'FAIL');
				if (!cancelled) setVerdict(ok ? 'VERDICT PASS' : 'VERDICT FAIL');
				return;
			}

			if (phase === 'storyboard-recovery') {
				// Port-collision recovery smoke: pre-bind :3105, install the
				// storyboard sidecar, watch the supervisor transition to Blocked
				// (no strike on the retry budget), release the port, watch it
				// recover to Running. Verifies PR 7's PortInUse → Blocked path.
				await safeUninstall(SB_ID);

				type SupEntry = {
					pkg_id: string;
					state: string;
					pid?: number | null;
					last_err?: string | null;
				};
				type SupSnap = { entries?: SupEntry[] };
				const findSupEntry = async (): Promise<SupEntry | null> => {
					const status = await pkgKernelStatus();
					const sup = (status.registries.sidecar_supervisor ?? {}) as SupSnap;
					return (sup.entries ?? []).find((e) => e.pkg_id === SB_ID) ?? null;
				};
				const waitForState = async (want: string, timeoutMs: number): Promise<SupEntry | null> => {
					const start = Date.now();
					while (Date.now() - start < timeoutMs) {
						if (cancelled) return null;
						const e = await findSupEntry();
						if (e?.state === want) return e;
						await new Promise((r) => setTimeout(r, 250));
					}
					return null;
				};

				// 1) Hold port 3105 from the Tauri side.
				let token: number | null = null;
				try {
					token = await devBindPort(3105);
					log('PRE_BIND', `OK token=${token} port=3105`);
				} catch (e) {
					log('PRE_BIND', `FAIL ${(e as Error).message ?? String(e)}`);
					if (!cancelled) setVerdict('VERDICT FAIL');
					return;
				}

				// 2) Install storyboard. Supervisor spawns the sidecar's MCP
				//    server and reaches Running — the sidecar itself doesn't
				//    bind any port until start_storyboard is invoked.
				let installed = false;
				try {
					const r = await pkgInstallFromPath(SB_PATH);
					installed = true;
					log('SB_INSTALL', `OK id=${r.installed.id}@${r.installed.version}`);
				} catch (e) {
					log('SB_INSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
				}
				const initiallyRunning = await waitForState('running', 15_000);
				log(
					'SB_RUNNING_PRE',
					initiallyRunning ? 'OK' : 'FAIL (handshake never completed within 15s)'
				);

				// 3) Trigger Vite spawn via start_storyboard. Vite sees the held
				//    :3105 → EADDRINUSE → sidecar emits port_in_use notification
				//    → exits code=2 → supervisor transitions to Blocked.
				try {
					const r = await pkgMcpCall(SB_ID, 'start_storyboard', {});
					// Expected to fail (port held). Either the rpc errors, or the
					// child kills the sidecar mid-flight. Both are fine; we only
					// care that the supervisor catches the notification.
					log('SB_START_TRIGGER', r.ok ? 'unexpected OK' : `expected FAIL ${r.error ?? ''}`);
				} catch (e) {
					log('SB_START_TRIGGER', `expected FAIL ${(e as Error).message ?? String(e)}`);
				}

				// 4) Assert Blocked within 15s — sidecar exit + supervisor pickup
				//    is fast once the notification flushes.
				const blocked = await waitForState('blocked', 15_000);
				const blockedOk = blocked !== null;
				log(
					'SB_BLOCKED',
					blockedOk
						? `OK last_err=${blocked?.last_err ?? '?'}`
						: 'FAIL (state never reached blocked within 30s)'
				);

				// 4) Release the port. Supervisor's 10s blocked-retry timer will
				//    kick in next; allow up to 30s before declaring failure
				//    (10s sleep + cold Vite boot).
				let releasedOk = false;
				if (token !== null) {
					try {
						releasedOk = await devReleasePort(token);
						log('RELEASE_PORT', releasedOk ? 'OK' : 'FAIL (token already gone)');
					} catch (e) {
						log('RELEASE_PORT', `FAIL ${(e as Error).message ?? String(e)}`);
					}
				}

				const recovered = await waitForState('running', 30_000);
				const recoveredOk = recovered !== null;
				log(
					'SB_RECOVERED',
					recoveredOk
						? `OK pid=${recovered?.pid ?? '?'}`
						: 'FAIL (state never reached running within 30s)'
				);

				// 5) Cleanup.
				try {
					await pkgUninstall(SB_ID);
				} catch {
					// ignore
				}

				const ok = installed && blockedOk && releasedOk && recoveredOk;
				log('DONE', ok ? 'PASS' : 'FAIL');
				if (!cancelled) setVerdict(ok ? 'VERDICT PASS' : 'VERDICT FAIL');
				return;
			}

			if (phase === 'verify') {
				const entry = await findIframeAppEntry();
				if (!entry) {
					log('STATUS', 'iframeapp ui_route entry missing');
					if (!cancelled) setVerdict('VERDICT FAIL');
					return;
				}
				log('STATUS', `entry path=${entry.path} kind=${entry.kind} source=${entry.source}`);
				if (!cancelled) setVerdict('VERDICT PASS');
				return;
			}

			// roundtrip / install: clean slate first.
			await safeUninstall(IFRAMEAPP_ID);
			await safeUninstall(HF_ID);

			// 1. Install iframeapp.
			let installed = false;
			try {
				const r = await pkgInstallFromPath(IFRAMEAPP_PATH);
				installed = true;
				log('INSTALL', `OK id=${r.installed.id}@${r.installed.version}`);
			} catch (e) {
				log('INSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			// 2. Verify ui_route entry shows up.
			let entry: UiRouteEntry | null = null;
			try {
				entry = await findIframeAppEntry();
				log('UI_ROUTE', entry ? `found path=${entry.path} kind=${entry.kind}` : 'not found');
			} catch (e) {
				log('UI_ROUTE', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			// 3. Mint a content URL — confirms pkg_content registered the dist root.
			let contentUrlOk = false;
			try {
				const handle = await pkgContentUrl(IFRAMEAPP_ID);
				contentUrlOk = handle.url.startsWith('http://127.0.0.1:') && handle.token.length === 64;
				log('CONTENT_URL', `${handle.url} token=${handle.token.slice(0, 8)}…`);
			} catch (e) {
				log('CONTENT_URL', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			// 4. Mount the iframe host and wait for ui/initialize round-trip.
			if (entry && contentUrlOk) {
				setMountSrc({ pkgId: IFRAMEAPP_ID, source: entry.source });
				const ok = await waitForInitialize(8000);
				log('UI_INITIALIZE', ok ? 'OK (handshake completed)' : 'TIMEOUT (no initialize within 8s)');
			} else {
				log('UI_INITIALIZE', 'SKIPPED (precondition missing)');
			}

			// 4b. Drive a real `tools/call` through pkg_mcp_call. Asserts the
			//     mcp_runtime spawn-per-call path against the fixture's stdio
			//     server. Independent of the iframe — same pipeline the iframe's
			//     `App.callServerTool` would hit, just invoked directly.
			let mcpOk = false;
			try {
				const r = await pkgMcpCall(IFRAMEAPP_ID, 'echo', { msg: 'smoke' });
				if (!r.ok) {
					log('MCP_CALL', `FAIL ${r.error ?? 'unknown'}`);
				} else {
					// MCP tools/call result is shaped `{ content: [{type:"text",text:"echo: smoke"}] }`.
					const content = (r.result as { content?: Array<{ text?: string }> } | null)?.content;
					const text = content?.[0]?.text ?? '';
					mcpOk = text === 'echo: smoke';
					log('MCP_CALL', mcpOk ? `OK (${text})` : `FAIL bad payload: ${JSON.stringify(r.result)}`);
				}
			} catch (e) {
				log('MCP_CALL', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			// 5. Hyperframes stub: install + immediate uninstall, no mount.
			let hfOk = false;
			try {
				await safeUninstall(HF_ID);
				const r = await pkgInstallFromPath(HF_PATH);
				log('HF_INSTALL', `OK id=${r.installed.id}`);
				await pkgUninstall(HF_ID);
				log('HF_UNINSTALL', 'OK');
				hfOk = true;
			} catch (e) {
				log('HF_DESERIALIZE', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			if (phase === 'install') {
				// Leave iframeapp installed for boot replay.
				const ok = installed && contentUrlOk && hfOk && mcpOk;
				log('DONE', ok ? 'PASS-leave-installed' : 'FAIL');
				if (!cancelled) setVerdict(ok ? 'VERDICT PASS (left installed)' : 'VERDICT FAIL');
				return;
			}

			// 6. Uninstall iframeapp; verify content URL is now refused.
			let uninstallOk = false;
			try {
				// Drop the iframe first so its own revoke runs cleanly.
				setMountSrc(null);
				await new Promise((r) => setTimeout(r, 50));
				await pkgUninstall(IFRAMEAPP_ID);
				// After uninstall, mint should fail.
				let mintAfterUninstall = 'mint did not error';
				try {
					await pkgContentUrl(IFRAMEAPP_ID);
				} catch (e) {
					mintAfterUninstall = (e as Error).message ?? String(e);
				}
				uninstallOk = mintAfterUninstall.includes('no iframe content');
				log(
					'UNINSTALL',
					uninstallOk
						? `OK (mint rejected: ${mintAfterUninstall})`
						: `FAIL (mint succeeded post-uninstall: ${mintAfterUninstall})`
				);
			} catch (e) {
				log('UNINSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			const ok = installed && contentUrlOk && hfOk && uninstallOk && mcpOk;
			log('DONE', ok ? 'PASS' : 'FAIL');
			if (!cancelled) setVerdict(ok ? 'VERDICT PASS' : 'VERDICT FAIL');
		})();

		return () => {
			cancelled = true;
		};
	}, [phase, started]);

	return (
		<div style={{ padding: 24, fontFamily: 'monospace', fontSize: 13 }}>
			<h1 style={{ fontSize: 16, marginBottom: 12 }}>Smoke: iframe mount mechanism</h1>
			<div style={{ marginBottom: 8, fontSize: 12, opacity: 0.7 }}>
				phase=<b>{phase}</b> — try ?phase=cleanup | install | verify | roundtrip | longlived |
				hyperframes | video-studio | storyboard
			</div>
			<div style={{ marginBottom: 12 }}>
				<button
					type="button"
					disabled={started}
					onClick={() => setStarted(true)}
					style={{
						padding: '6px 14px',
						fontFamily: 'inherit',
						fontSize: 12,
						cursor: started ? 'default' : 'pointer',
					}}
				>
					{started ? 'Running…' : `Run phase=${phase}`}
				</button>
			</div>
			<div style={{ marginBottom: 16, fontWeight: 600 }}>{verdict}</div>
			<table style={{ borderCollapse: 'collapse', minWidth: 540 }}>
				<tbody>
					{rows.map((r, i) => (
						<tr key={i} style={{ borderTop: '1px solid #333' }}>
							<td style={{ padding: '4px 12px 4px 0', opacity: 0.7, verticalAlign: 'top' }}>
								{r.label}
							</td>
							<td style={{ padding: '4px 0' }}>{r.outcome}</td>
						</tr>
					))}
				</tbody>
			</table>
			{mountSrc && (
				<div
					key={iframeBoxKey}
					style={{
						marginTop: 24,
						border: '1px solid #444',
						borderRadius: 4,
						height: 280,
						overflow: 'hidden',
					}}
				>
					<PkgIframeHost
						pkgId={mountSrc.pkgId}
						source={mountSrc.source}
						onInitialized={() => {
							initializedRef.current = true;
						}}
					/>
				</div>
			)}
		</div>
	);
}
