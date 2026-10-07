// plans/pwa S4 §3 / Shape 3–4: Settings › Notifications.
//
// - "Push to this device": what this browser can do (`pushState`), a Turn on
//   button — the only place besides the phone nudge that asks for
//   permission, and only on a click — and per-event toggles.
// - "Send a test".
// - "Devices getting notifications": the caller's own subscriptions (host
//   only, never the endpoint path), each removable.
//
// The desktop app shows one line: notifications reach other browsers, and the
// desktop never registers a service worker.

import { Bell, BellOff, Send, Trash2 } from 'lucide-react';
import { useCallback, useEffect, useMemo, useState } from 'react';

import { Banner } from '@/components/ui/banner';
import { Button } from '@/components/ui/button';
import { StatusChip } from '@/components/ui/status-chip';
import { Switch } from '@/components/ui/switch';
import {
	accessPushConfig,
	accessPushList,
	accessPushTest,
	accessPushUnsubscribe,
	accessPushUpdate,
	parseAccessError,
	type PushConfig,
	type PushKind,
	type PushSubscriptionView,
} from '@/lib/access/client';
import { currentPushEnv, pushMessage, pushState } from '@/lib/pwa/platform';
import { disablePush, enablePush, readStoredSub, type StoredSub } from '@/lib/pwa/push-client';
import { isTauri } from '@/lib/transport';
import { SettingGroup } from '@/routes/settings/-components/setting-group';
import { SettingRow } from '@/routes/settings/-components/setting-row';

/** The toggles, grouped the way people think about them. */
export const PUSH_GROUPS: ReadonlyArray<{
	id: string;
	label: string;
	desc: string;
	kinds: PushKind[];
}> = [
	{
		id: 'approvals',
		label: 'Approvals needed',
		desc: 'A Chi is waiting for you to allow or deny something.',
		kinds: ['permission'],
	},
	{
		id: 'runs',
		label: 'Chi runs',
		desc: 'A run finished, failed or was cancelled.',
		kinds: ['run_finished', 'run_failed', 'run_cancelled'],
	},
	{
		id: 'people',
		label: 'Invites and pairing requests',
		desc: 'Someone joined from your invite, or a device is asking to pair.',
		kinds: ['invite', 'pairing'],
	},
	{
		id: 'updates',
		label: 'Server updates',
		desc: 'A new Ikenga server release is available (admins).',
		kinds: ['update'],
	},
];

/** The groups this credential can receive at all. */
export function visibleGroups(allowed: readonly PushKind[]) {
	return PUSH_GROUPS.filter((g) => g.kinds.some((k) => allowed.includes(k)));
}

/** `kinds` with one group switched on or off, limited to `allowed`. */
export function toggleGroup(
	kinds: readonly PushKind[],
	group: { kinds: PushKind[] },
	on: boolean,
	allowed: readonly PushKind[]
): PushKind[] {
	const rest = kinds.filter((k) => !group.kinds.includes(k));
	const add = on ? group.kinds.filter((k) => allowed.includes(k)) : [];
	return [...rest, ...add];
}

function relative(ms: number | null): string {
	if (!ms) return 'never';
	const s = Math.round((Date.now() - ms) / 1000);
	if (s < 60) return 'just now';
	if (s < 3600) return `${Math.round(s / 60)} min ago`;
	if (s < 86_400) return `${Math.round(s / 3600)} h ago`;
	return `${Math.round(s / 86_400)} d ago`;
}

export function NotificationsSettings() {
	const tauri = isTauri();
	const state = pushState(currentPushEnv(tauri));
	if (tauri) {
		return (
			<Shell>
				<SettingGroup title="Push notifications">
					<SettingRow label="This computer" desc={pushMessage('tauri')}>
						<StatusChip tone="muted">desktop app</StatusChip>
					</SettingRow>
				</SettingGroup>
			</Shell>
		);
	}
	return (
		<Shell>
			<BrowserPush state={state} />
		</Shell>
	);
}

function Shell({ children }: { children: React.ReactNode }) {
	return (
		<div
			data-state="notifications-settings"
			className="mx-auto w-full max-w-[860px] space-y-4 px-6 py-6"
		>
			{children}
		</div>
	);
}

function BrowserPush({ state }: { state: ReturnType<typeof pushState> }) {
	const [config, setConfig] = useState<PushConfig | null>(null);
	const [rows, setRows] = useState<PushSubscriptionView[]>([]);
	const [stored, setStored] = useState<StoredSub | null>(() => readStoredSub());
	const [busy, setBusy] = useState(false);
	const [note, setNote] = useState<string | null>(null);
	const [error, setError] = useState<string | null>(null);

	const refresh = useCallback(async () => {
		try {
			const [c, list] = await Promise.all([accessPushConfig(), accessPushList()]);
			setConfig(c);
			setRows(list);
			setStored(readStoredSub());
			setError(null);
		} catch (e) {
			setError(parseAccessError(e).message);
		}
	}, []);

	useEffect(() => {
		void refresh();
	}, [refresh]);

	const allowed = useMemo<PushKind[]>(() => (config?.enabled ? config.kinds : []), [config]);
	const mine = rows.find((r) => r.subId === stored?.subId) ?? null;
	const on = mine !== null;
	const message = pushMessage(state);

	const turnOn = async () => {
		setBusy(true);
		setNote(null);
		// `enablePush` asks for permission first, inside this click.
		const r = await enablePush();
		setBusy(false);
		if (!r.ok) setNote(r.message);
		await refresh();
	};

	const turnOff = async () => {
		setBusy(true);
		await disablePush();
		setBusy(false);
		await refresh();
	};

	const setGroup = async (group: (typeof PUSH_GROUPS)[number], value: boolean) => {
		if (!mine) return;
		const next = toggleGroup(mine.kinds, group, value, allowed);
		setRows((rs) => rs.map((r) => (r.subId === mine.subId ? { ...r, kinds: next } : r)));
		try {
			await accessPushUpdate(mine.subId, next);
		} catch (e) {
			setNote(parseAccessError(e).message);
			await refresh();
		}
	};

	const sendTest = async () => {
		if (!mine) return;
		try {
			await accessPushTest(mine.subId);
			setNote('Test sent. It should arrive in a few seconds.');
		} catch (e) {
			setNote(parseAccessError(e).message);
		}
	};

	const remove = async (row: PushSubscriptionView) => {
		if (row.subId === stored?.subId) {
			await turnOff();
			return;
		}
		await accessPushUnsubscribe({ subId: row.subId }).catch((e) =>
			setNote(parseAccessError(e).message)
		);
		await refresh();
	};

	return (
		<>
			{error && (
				<Banner tone="warning" icon={<BellOff />}>
					{error}
				</Banner>
			)}
			{config && !config.enabled && (
				<Banner tone="info" icon={<BellOff />}>
					{config.reason ?? 'Push notifications are off on this server.'}
				</Banner>
			)}
			<SettingGroup title="Push to this device">
				<SettingRow
					label="Notifications on this device"
					desc={
						message ??
						(on
							? 'This browser gets the events switched on below, even when Ikenga is closed.'
							: 'Get a notification when something needs you, even when Ikenga is closed. Only the kind of event is sent; details stay on your server.')
					}
				>
					{state !== 'ready' ? (
						<StatusChip tone="warn">{state === 'denied' ? 'blocked' : 'unavailable'}</StatusChip>
					) : on ? (
						<Button size="sm" variant="outline" disabled={busy} onClick={() => void turnOff()}>
							<BellOff /> Turn off
						</Button>
					) : (
						<Button
							size="sm"
							disabled={busy || !config?.enabled}
							onClick={() => void turnOn()}
							data-action="push-turn-on"
						>
							<Bell /> Turn on
						</Button>
					)}
				</SettingRow>
				{visibleGroups(allowed).map((g) => {
					const checked = mine ? g.kinds.some((k) => mine.kinds.includes(k)) : false;
					return (
						<SettingRow key={g.id} label={g.label} desc={g.desc}>
							<Switch
								aria-label={g.label}
								data-push-group={g.id}
								checked={checked}
								disabled={!on}
								onCheckedChange={(v) => void setGroup(g, v)}
							/>
						</SettingRow>
					);
				})}
				<SettingRow label="Send a test" desc="Check that notifications reach this device.">
					<Button size="sm" variant="outline" disabled={!on} onClick={() => void sendTest()}>
						<Send /> Send a test
					</Button>
				</SettingRow>
				{note && (
					<p role="status" className="m-0 px-4 py-2 text-xs text-muted-foreground">
						{note}
					</p>
				)}
			</SettingGroup>

			<SettingGroup title="Devices getting notifications">
				{rows.length === 0 && (
					<p className="m-0 px-4 py-3 text-xs text-muted-foreground">
						No device gets notifications yet.
					</p>
				)}
				{rows.map((r) => (
					<SettingRow
						key={r.subId}
						label={
							<span className="flex items-center gap-2">
								{r.label ?? r.endpointHost}
								{r.subId === stored?.subId && <StatusChip tone="live">this device</StatusChip>}
							</span>
						}
						desc={`${r.endpointHost} · last delivered ${relative(r.lastSuccessAt)}`}
					>
						<Button
							size="sm"
							variant="outline"
							aria-label={`Remove ${r.label ?? r.endpointHost}`}
							onClick={() => void remove(r)}
						>
							<Trash2 /> Remove
						</Button>
					</SettingRow>
				))}
			</SettingGroup>
		</>
	);
}
