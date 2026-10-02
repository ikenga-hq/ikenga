// "Permission requests" — D-05 `devices` segmented control (G-ACCESS §5.1,
// DEC-79; WP-75). Who may answer Chi's asks:
//
// - "This device only" (`this_device`): only the device you set it from —
//   on the desktop that is the host itself (D-05: ned-desktop). Every other
//   device sees the ask and who it is waiting on, read-only.
// - "Any paired device" (`any_approve`, the default): any device of yours
//   whose capability includes approve. A device can never approve beyond its
//   own grant (§5.1, capped by grants).
//
// Changing it needs an admin-strength credential (the host, a password
// session or a `full` device), is audited (`routing.changed`) and makes the
// affected devices reconnect. The daemon decides; this control only reflects
// it (§1.5).

import { useCallback, useEffect, useState } from 'react';

import { Segmented } from '@/components/ui/segmented';
import { TIER_CAPS, TIER_LABELS } from '@/lib/access/caps.gen';
import {
	type AccessStatus,
	accessDevicesList,
	accessRoutingGet,
	accessRoutingSet,
	accessStatus,
	type DeviceView,
	parseAccessError,
	type RoutingMode,
} from '@/lib/access/client';

/** `access_routing_get`'s result; `deviceName` is WP-75's additive field. */
export interface RoutingView {
	mode: RoutingMode;
	deviceId: string | null;
	deviceName?: string | null;
}

export type RoutingNote =
	| { kind: 'here'; device: string }
	| { kind: 'elsewhere'; device: string | null }
	| { kind: 'any'; approvers: string[] };

/** Devices that may answer under "any paired device" (tier holds approve). */
export function approverNames(devices: readonly DeviceView[]): string[] {
	return devices.filter((d) => TIER_CAPS[d.tier].includes('approve')).map((d) => d.name);
}

/** What the rule box under the control says (D-05 `approveNote`). */
export function routingNote(
	pref: RoutingView,
	devices: readonly DeviceView[],
	thisDeviceId: string | null
): RoutingNote {
	if (pref.mode === 'this_device') {
		const named = devices.find((d) => d.deviceId === pref.deviceId);
		const name = pref.deviceName ?? named?.name ?? null;
		if (pref.deviceId && pref.deviceId === thisDeviceId) {
			return { kind: 'here', device: name ?? 'this device' };
		}
		return { kind: 'elsewhere', device: name };
	}
	return { kind: 'any', approvers: approverNames(devices) };
}

/** Why the control is read-only for this credential, or `null`. */
export function routingDisabledReason(status: AccessStatus | null): string | null {
	if (!status) return null;
	if (status.store !== 'ok') {
		return status.store === 'degraded'
			? 'The audit chain is broken — access changes are paused.'
			: 'No access store: start ikenga-server with a data directory.';
	}
	if (!status.adminStrength) {
		return `Only the host, a signed-in browser or a Full device can change this — this device is ${TIER_LABELS[status.credential.tier].label}.`;
	}
	return null;
}

/** "This device only" needs a device that can approve (§5.1). */
export function thisDeviceDisabledReason(status: AccessStatus | null): string | null {
	if (!status) return null;
	if (!status.credential.deviceId) return 'A signed-in browser is not a paired device.';
	if (!TIER_CAPS[status.credential.tier].includes('approve')) {
		return `This device can't approve — it is ${TIER_LABELS[status.credential.tier].label}.`;
	}
	return null;
}

function joinNames(names: string[]): string {
	if (names.length <= 1) return names[0] ?? '';
	return `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`;
}

export function RoutingPolicy() {
	const [status, setStatus] = useState<AccessStatus | null>(null);
	const [pref, setPref] = useState<RoutingView | null>(null);
	const [devices, setDevices] = useState<DeviceView[]>([]);
	const [loadError, setLoadError] = useState<string | null>(null);
	const [saving, setSaving] = useState(false);
	const [error, setError] = useState<string | null>(null);

	const load = useCallback(async () => {
		try {
			const [st, rp, dv] = await Promise.all([
				accessStatus(),
				accessRoutingGet() as Promise<RoutingView>,
				accessDevicesList().catch(() => [] as DeviceView[]),
			]);
			setStatus(st ?? null);
			setPref(rp ?? null);
			setDevices(Array.isArray(dv) ? dv : []);
			setLoadError(null);
		} catch (e) {
			setLoadError(parseAccessError(e).message);
		}
	}, []);

	useEffect(() => {
		void load();
	}, [load]);

	const change = async (mode: string) => {
		if (mode !== 'this_device' && mode !== 'any_approve') return;
		if (pref?.mode === mode && mode === 'any_approve') return;
		setSaving(true);
		setError(null);
		try {
			const next = (await accessRoutingSet(mode)) as RoutingView;
			setPref(next);
			await load();
		} catch (e) {
			setError(parseAccessError(e).message);
		} finally {
			setSaving(false);
		}
	};

	if (loadError || !pref) {
		return (
			<div
				data-routing={loadError ? 'unavailable' : 'loading'}
				className="flex min-w-0 flex-col gap-2"
			>
				<Segmented
					ariaLabel="Who may answer Chi's asks"
					value="any_approve"
					onValueChange={() => {}}
					items={[
						{ id: 'this_device', label: 'This device only', disabled: true },
						{ id: 'any_approve', label: 'Any paired device', disabled: true },
					]}
				/>
				<span className="text-[var(--text-micro)] text-[var(--fg-muted)]">
					{loadError
						? `Can't read who may answer asks: ${loadError}`
						: 'Reading who may answer asks…'}
				</span>
			</div>
		);
	}

	const disabled = routingDisabledReason(status);
	const hereDisabled = thisDeviceDisabledReason(status);
	const thisDeviceId = status?.credential.deviceId ?? null;
	const note = routingNote(pref, devices, thisDeviceId);
	const b = 'font-semibold text-[var(--fg)]';

	return (
		<div data-routing={pref.mode} className="flex min-w-0 flex-col gap-2">
			<Segmented
				ariaLabel="Who may answer Chi's asks"
				value={pref.mode}
				onValueChange={(id) => void change(id)}
				items={[
					{
						id: 'this_device',
						label: 'This device only',
						disabled: saving || disabled !== null || hereDisabled !== null,
					},
					{ id: 'any_approve', label: 'Any paired device', disabled: saving || disabled !== null },
				]}
			/>
			<span
				data-note={note.kind}
				className="rounded-md border border-dashed border-[var(--border)] px-3 py-1.5 text-[var(--text-micro)] text-[var(--fg-muted)]"
			>
				{note.kind === 'here' && (
					<>
						Asks are answered on <b className={b}>{note.device}</b> only. A remote client sees the
						ask and who it is waiting on, and can do nothing about it.
					</>
				)}
				{note.kind === 'elsewhere' && (
					<>
						Asks are answered on{' '}
						<b className={b}>{note.device ?? 'a device that is no longer paired'}</b> only.{' '}
						{note.device
							? 'Every other device, this one included, sees the ask read-only.'
							: 'Nobody can answer until you choose again.'}
					</>
				)}
				{note.kind === 'any' && (
					<>
						Any device with <b className={b}>approve</b> may answer.
						{note.approvers.length > 0 && (
							<>
								{' '}
								Right now that is <b className={b}>{joinNames(note.approvers)}</b>.
							</>
						)}{' '}
						A device can never approve more than its own capability allows.
					</>
				)}
			</span>
			{(disabled || (hereDisabled && pref.mode !== 'this_device')) && (
				<span className="text-[var(--text-micro)] text-[var(--fg-muted)]">
					{disabled ?? hereDisabled}
				</span>
			)}
			{error && (
				<span role="alert" className="text-[var(--text-micro)] text-[var(--danger)]">
					{error}
				</span>
			)}
		</div>
	);
}
