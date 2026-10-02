// Devices tab — D-05 `devices` (`designs/people.html?state=devices`).
// WP-72 shipped the read-only Remote access block; WP-74b (G-ACCESS §3) adds
// pairing and the live devices table.
//
// - Remote access and Perimeter stay read-only: exposure is a daemon flag
//   (§15 N-6, "Set by how the server was started").
// - Permission requests: `routing-policy.tsx` (a stub here; WP-75 fills it).
// - Devices: the access store's rows (`access_devices_list`): last seen,
//   address, the capability menu (§1.3, `access_device_set_tier`), live
//   sessions, and Revoke (§3.10: immediate, no Undo — D-5 / P-17).
// - The shared bearer token is no longer presented as a way to connect: every
//   remote device holds its own grant (§15 N-7).

import { ChevronDown, Plus, RefreshCw } from 'lucide-react';
import { useCallback, useEffect, useState } from 'react';

import { Button } from '@/components/ui/button';
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import {
	DropdownMenu,
	DropdownMenuContent,
	DropdownMenuRadioGroup,
	DropdownMenuRadioItem,
	DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { StatusChip } from '@/components/ui/status-chip';
import { TIER_LABELS, type Tier } from '@/lib/access/caps.gen';
import {
	accessDeviceRevoke,
	accessDeviceSetTier,
	accessDevicesList,
	type DeviceView,
	parseAccessError,
} from '@/lib/access/client';
import { type DaemonInfo, isRemoteWebSession, ptyDaemonInfo } from '@/lib/tauri-cmd';

import {
	buildDevicesView,
	type DevicesView,
	deviceSubLine,
	EXPOSURE_COPY,
	type ProbeResult,
	pairedCount,
	probeDaemon,
	relativeTime,
	tierChoices,
} from './devices-model';
import { usePairWatch } from './devices-pair-confirm';
import { PairSheet } from './devices-pair-sheet';
import { Kv, PeopleBlock, PeopleFileBar, PeopleHeader, PeopleRow } from './frame';
import { RoutingPolicy } from './routing-policy';

export function DevicesTab() {
	const { view, refreshing, refresh } = useDevicesView();
	const devices = useDevices();
	const [pairing, setPairing] = useState(false);
	return (
		<div data-state="devices" className="mx-auto w-full max-w-[960px] space-y-4 px-6 py-6">
			<PeopleHeader tab="devices" />
			<RemoteAccessBlock view={view} refreshing={refreshing} onRefresh={refresh} />
			<DevicesTableBlock devices={devices} onPair={() => setPairing(true)} />
			<PairSheet open={pairing} onOpenChange={setPairing} view={view} />
			<PeopleFileBar />
		</div>
	);
}

function useDevicesView() {
	const [info, setInfo] = useState<DaemonInfo | null>(null);
	const [probe, setProbe] = useState<ProbeResult>('skipped');
	const [refreshing, setRefreshing] = useState(false);
	const remote = isRemoteWebSession();

	const refresh = useCallback(async () => {
		setRefreshing(true);
		try {
			if (remote) {
				setProbe(await probeDaemon(window.location.origin));
				return;
			}
			const next = await ptyDaemonInfo();
			setInfo(next);
			if (next?.available && next.mode === 'persistent' && next.httpUrl) {
				setProbe(await probeDaemon(next.httpUrl));
			} else {
				setProbe('skipped');
			}
		} finally {
			setRefreshing(false);
		}
	}, [remote]);

	useEffect(() => {
		void refresh();
	}, [refresh]);

	const view = buildDevicesView(
		remote ? null : info,
		remote
			? {
					origin: window.location.origin,
					hostname: window.location.hostname,
					hasToken: true,
				}
			: null,
		probe
	);
	return { view, refreshing, refresh };
}

interface DevicesState {
	rows: DeviceView[] | null;
	error: string | null;
	reload: () => Promise<void>;
}

function useDevices(): DevicesState {
	const [rows, setRows] = useState<DeviceView[] | null>(null);
	const [error, setError] = useState<string | null>(null);
	const revision = usePairWatch((s) => s.revision);
	const reload = useCallback(async () => {
		try {
			setRows(await accessDevicesList());
			setError(null);
		} catch (e) {
			const { code, message } = parseAccessError(e);
			setRows(null);
			setError(
				code === 'store_unavailable'
					? "Paired devices live in the background server's access store, and it isn't available right now (no ikenga-server, or it runs without a data folder)."
					: message
			);
		}
	}, []);
	// biome-ignore lint/correctness/useExhaustiveDependencies: `revision` is the refresh signal (a device was just paired)
	useEffect(() => {
		void reload();
		const t = setInterval(() => void reload(), 15_000);
		return () => clearInterval(t);
	}, [reload, revision]);
	return { rows, error, reload };
}

const RUN_TONE = { running: 'live', stopped: 'muted', unknown: 'warn' } as const;

function RemoteAccessBlock({
	view,
	refreshing,
	onRefresh,
}: {
	view: DevicesView;
	refreshing: boolean;
	onRefresh: () => void;
}) {
	const exposure = EXPOSURE_COPY[view.exposure];
	const serving = view.run === 'running' && view.address !== null;
	return (
		<PeopleBlock
			title="Remote access"
			right={
				<>
					<Kv>daemon · {view.run}</Kv>
					<Button
						type="button"
						variant="ghost"
						size="icon-xs"
						aria-label="Check again"
						title="Check again"
						disabled={refreshing}
						onClick={onRefresh}
					>
						<RefreshCw className={refreshing ? 'animate-spin' : undefined} />
					</Button>
				</>
			}
		>
			<PeopleRow
				label="Reachable from elsewhere"
				sub="Serves this workspace over HTTP + WebSocket from the machine it runs on."
			>
				<StatusChip tone={RUN_TONE[view.run]} dot>
					{view.run}
				</StatusChip>
				<Kv className="break-all">{serving ? view.address : 'not serving'}</Kv>
				<Kv className="basis-full">{view.runNote}</Kv>
			</PeopleRow>
			<PeopleRow
				label="Perimeter"
				sub="Who can reach the address at all, before any device is paired."
			>
				<StatusChip tone={exposure.tone}>{exposure.label}</StatusChip>
				<Kv className="basis-full">
					{view.address ? exposure.note : 'Nothing is listening, so nothing is exposed.'}
				</Kv>
				<Kv className="basis-full">Set by how the server was started.</Kv>
			</PeopleRow>
			<PeopleRow label="Tailnet address">
				{view.tailnetAddress ? (
					<Kv className="text-[var(--fg)]">{view.tailnetAddress}</Kv>
				) : (
					<>
						<Kv>not on a tailnet</Kv>
						<Kv className="basis-full">
							To reach it from your other devices, run <code>ikenga-server --host</code> with this
							machine's tailnet address, or put it behind <code>tailscale serve</code>.
						</Kv>
					</>
				)}
			</PeopleRow>
			<PeopleRow label="Permission requests" sub="Who may answer Chi's asks." top>
				<RoutingPolicy />
			</PeopleRow>
			{view.source === 'desktop' && (
				<PeopleRow label="Process">
					<Kv>
						{view.mode === 'persistent' ? 'ikenga-server' : 'in-process (no daemon)'}
						{view.pid !== null && ` · pid ${view.pid}`}
					</Kv>
				</PeopleRow>
			)}
		</PeopleBlock>
	);
}

function DevicesTableBlock({ devices, onPair }: { devices: DevicesState; onPair: () => void }) {
	const [now, setNow] = useState(() => Date.now());
	const [revoking, setRevoking] = useState<DeviceView | null>(null);
	const [rowError, setRowError] = useState<string | null>(null);
	useEffect(() => {
		const t = setInterval(() => setNow(Date.now()), 30_000);
		return () => clearInterval(t);
	}, []);

	const setTier = async (d: DeviceView, tier: Tier) => {
		setRowError(null);
		try {
			await accessDeviceSetTier(d.deviceId, tier);
			await devices.reload();
		} catch (e) {
			setRowError(parseAccessError(e).message);
		}
	};

	const rows = devices.rows ?? [];
	return (
		<PeopleBlock
			title={
				<>
					Devices
					{devices.rows && <Kv>{pairedCount(rows)} paired</Kv>}
				</>
			}
			right={
				<Button
					type="button"
					variant="outline"
					size="xs"
					onClick={onPair}
					disabled={devices.rows === null}
					title={devices.rows === null ? (devices.error ?? undefined) : undefined}
				>
					<Plus /> Pair a device
				</Button>
			}
		>
			<div className="-mx-3 overflow-x-auto">
				<table className="w-full border-collapse text-left text-[var(--text-caption,12px)]">
					<thead>
						<tr className="border-b border-[var(--border-soft)] text-[var(--text-micro)] uppercase tracking-[0.08em] text-[var(--fg-muted)]">
							<th className="px-3 py-2 font-semibold">Device</th>
							<th className="px-3 py-2 font-semibold">Last seen</th>
							<th className="px-3 py-2 font-semibold">Address</th>
							<th className="px-3 py-2 font-semibold">What it can do</th>
							<th className="px-3 py-2 font-semibold">Live sessions</th>
							<th className="px-3 py-2 text-right font-semibold">
								<span className="sr-only">Actions</span>
							</th>
						</tr>
					</thead>
					<tbody>
						{rows.map((d) => (
							<tr
								key={d.deviceId}
								data-device-kind={d.kind}
								className="border-b border-[var(--border-soft)] last:border-b-0"
							>
								<td className="px-3 py-2">
									<span className="block text-[var(--fg)]">{d.name}</span>
									<span className="block text-[var(--text-micro)] text-[var(--fg-muted)]">
										{deviceSubLine(d)}
									</span>
								</td>
								<td className="px-3 py-2 text-[var(--fg-muted)]">
									{d.kind === 'host' || d.thisDevice ? 'now' : relativeTime(d.lastSeenAt, now)}
								</td>
								<td className="px-3 py-2 font-mono text-[var(--text-micro)] text-[var(--fg-muted)]">
									{d.kind === 'host' ? 'local' : (d.lastSeenAddr ?? '—')}
								</td>
								<td className="px-3 py-2">
									<TierMenu device={d} onPick={(t) => void setTier(d, t)} />
								</td>
								<td className="px-3 py-2 font-mono text-[var(--fg-muted)]">
									{d.liveSockets > 0 ? d.liveSockets : '–'}
								</td>
								<td className="px-3 py-2 text-right">
									{d.kind === 'host' || d.thisDevice ? (
										<Kv>this device</Kv>
									) : (
										<Button
											type="button"
											variant="outline"
											size="xs"
											className="text-[var(--danger)]"
											onClick={() => setRevoking(d)}
										>
											Revoke
										</Button>
									)}
								</td>
							</tr>
						))}
						{devices.rows !== null && pairedCount(rows) === 0 && (
							<tr>
								<td colSpan={6} className="px-3 py-4 text-[var(--fg-muted)]">
									No paired devices. Pair a phone or another computer to answer and instruct Chi
									from it — each device gets its own grant, and you can revoke it on its own.
								</td>
							</tr>
						)}
						{devices.rows === null && (
							<tr>
								<td
									colSpan={6}
									className="px-3 py-4 text-[var(--fg-muted)]"
									data-devices="unavailable"
								>
									{devices.error ?? 'Loading…'}
								</td>
							</tr>
						)}
					</tbody>
				</table>
			</div>
			{rowError && (
				<p role="alert" className="m-0 py-2 text-[12px] text-[var(--danger)]">
					{rowError}
				</p>
			)}
			<RevokeConfirm
				device={revoking}
				onClose={() => setRevoking(null)}
				onDone={() => void devices.reload()}
			/>
		</PeopleBlock>
	);
}

function TierMenu({ device, onPick }: { device: DeviceView; onPick: (t: Tier) => void }) {
	const label = TIER_LABELS[device.tier].label;
	if (device.kind === 'host') {
		return (
			<span title="This device is where everything runs — it is always Full.">
				<StatusChip tone="muted">{label}</StatusChip>
			</span>
		);
	}
	return (
		<DropdownMenu>
			<DropdownMenuTrigger asChild>
				<Button type="button" variant="outline" size="xs" aria-label={`What ${device.name} can do`}>
					{label} <ChevronDown />
				</Button>
			</DropdownMenuTrigger>
			<DropdownMenuContent align="start" className="w-[300px]">
				<DropdownMenuRadioGroup value={device.tier} onValueChange={(v) => onPick(v as Tier)}>
					{tierChoices().map((t) => (
						<DropdownMenuRadioItem key={t.id} value={t.id} className="flex-col items-start">
							<span>{t.label}</span>
							<span className="text-[11px] text-[var(--fg-muted)]">{t.long}</span>
						</DropdownMenuRadioItem>
					))}
				</DropdownMenuRadioGroup>
			</DropdownMenuContent>
		</DropdownMenu>
	);
}

/** §3.10: revoke is immediate and final — no Undo (D-5, P-17). */
function RevokeConfirm({
	device,
	onClose,
	onDone,
}: {
	device: DeviceView | null;
	onClose: () => void;
	onDone: () => void;
}) {
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const revoke = async () => {
		if (!device) return;
		setBusy(true);
		setError(null);
		try {
			await accessDeviceRevoke(device.deviceId);
			onDone();
			onClose();
		} catch (e) {
			setError(parseAccessError(e).message);
		} finally {
			setBusy(false);
		}
	};
	return (
		<Dialog open={device !== null} onOpenChange={(o) => !o && onClose()}>
			<DialogContent
				data-state="devices-revoke"
				className="bg-[var(--bg-surface)] text-[var(--fg)]"
			>
				<DialogHeader>
					<DialogTitle>Revoke {device?.name}?</DialogTitle>
					<DialogDescription>
						It loses access at once and its open connections drop. The session itself keeps running.
						To use it again, pair it again.
					</DialogDescription>
				</DialogHeader>
				{error && (
					<p role="alert" className="m-0 text-[12px] text-[var(--danger)]">
						{error}
					</p>
				)}
				<DialogFooter>
					<Button type="button" variant="outline" size="sm" onClick={onClose}>
						Keep it
					</Button>
					<Button
						type="button"
						variant="destructive"
						size="sm"
						disabled={busy}
						onClick={() => void revoke()}
					>
						Revoke
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}
