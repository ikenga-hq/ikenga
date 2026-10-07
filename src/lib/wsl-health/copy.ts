// honest-failure-states WP-2 — what the shell says about a `WslHealth`
// result, and which fix it offers. One mapping, shared by the terminal-pane
// banner, Settings › Engines and the `fix.wsl_network` notification buttons,
// so the three surfaces (D-2) never disagree. Pure; unit-tested.
//
// Fix ladder, least → most disruptive (plan WP-2, D-1, D-6):
//   dns_only     → repair_dns (rewrites /etc/resolv.conf; nothing restarts)
//   no_route     → restart_networking (UAC; restarts HNS + shuts WSL down —
//                  a bare `wsl --shutdown` does not fix a failed mirrored setup)
//   wsl_down     → restart_networking
//   mirrored setup still failing after a restart → switch_to_nat (D-6)
//   host_offline → no fix: the PC itself is offline
//   not_installed / ok → nothing to say

import type { WslFixAction, WslHealth, WslHealthState } from '@/lib/tauri-cmd';
import { wslDistroLabel } from './tabs';

export type WslHealthTone = 'warning' | 'danger' | 'info';

export interface WslFixChoice {
	action: WslFixAction;
	label: string;
	/** Shuts every WSL distro down: confirm first, relaunch after (D-5). */
	disruptive: boolean;
}

export interface WslHealthCopy {
	tone: WslHealthTone;
	/** One plain-language sentence naming the cause. */
	title: string;
	/** What it means / what the fix does. */
	body: string;
	primary: WslFixChoice | null;
	secondary: WslFixChoice[];
}

const FIX: Record<WslFixAction, WslFixChoice> = {
	repair_dns: { action: 'repair_dns', label: 'Repair DNS', disruptive: false },
	restart_networking: {
		action: 'restart_networking',
		label: 'Restart WSL networking (needs admin)',
		disruptive: true,
	},
	switch_to_nat: { action: 'switch_to_nat', label: 'Switch to NAT…', disruptive: true },
};

export function wslFixChoice(action: WslFixAction): WslFixChoice {
	return FIX[action];
}

/** The fix a notification's `state` leads with (no health result needed). */
export function primaryFixForState(state: WslHealthState): WslFixChoice | null {
	switch (state) {
		case 'dns_only':
			return FIX.repair_dns;
		case 'no_route':
		case 'wsl_down':
			return FIX.restart_networking;
		default:
			return null;
	}
}

/**
 * The fix a `fix.wsl_network` notification leads with — the same one the
 * banner and Settings row show for that episode: taken from the cached
 * health when it reports the row's state, else from the state alone, where a
 * restart that already failed this episode moves `no_route` to NAT (D-6).
 */
export function notificationFix(
	state: WslHealthState,
	ctx: { health?: WslHealth | null; restartTried?: boolean } = {}
): WslFixChoice | null {
	if (ctx.health && ctx.health.state === state) {
		return wslHealthCopy(ctx.health, { restartTried: ctx.restartTried })?.primary ?? null;
	}
	if (ctx.restartTried && state === 'no_route') return FIX.switch_to_nat;
	return primaryFixForState(state);
}

export interface WslHealthCopyContext {
	/** A restart was already tried this episode and the mirrored setup still
	 *  failed — lead with NAT instead (D-6). */
	restartTried?: boolean;
}

function mirroredCause(health: WslHealth): string | null {
	if (!health.mirroredFailure) return null;
	const code = health.mirroredFailure.errorCode;
	return code
		? `Windows couldn't set up mirrored networking (${code})`
		: `Windows couldn't set up mirrored networking`;
}

/** Copy for a health result, or `null` when there is nothing to say. */
export function wslHealthCopy(
	health: WslHealth,
	ctx: WslHealthCopyContext = {}
): WslHealthCopy | null {
	const where = wslDistroLabel(health.distro);
	const mirrored = health.networkingMode === 'mirrored' || health.mirroredFailure != null;
	const natOffer = mirrored ? [FIX.switch_to_nat] : [];

	switch (health.state) {
		case 'ok':
		case 'not_installed':
			return null;

		case 'host_offline':
			return {
				tone: 'info',
				title: 'Your computer is offline',
				body: `WSL can't reach the internet because Windows can't either. Sessions will work again once this PC is back online.`,
				primary: null,
				secondary: [],
			};

		case 'no_route': {
			const cause = mirroredCause(health);
			const title = cause
				? `WSL started without a network connection — ${cause}`
				: `WSL started without a network connection`;
			if (ctx.restartTried && mirrored) {
				return {
					tone: 'danger',
					title,
					body: `Restarting WSL networking didn't bring mirrored networking back. Switching ${where} to NAT networking usually does.`,
					primary: FIX.switch_to_nat,
					secondary: [FIX.restart_networking],
				};
			}
			return {
				tone: 'danger',
				title,
				body: `Nothing inside ${where} can reach the internet, so sign-in and installs fail. Restarting WSL networking usually fixes this; it asks for administrator approval and closes every WSL session, which Ikenga reopens afterwards.`,
				primary: FIX.restart_networking,
				secondary: natOffer,
			};
		}

		case 'dns_only':
			return {
				tone: 'warning',
				title: `WSL is online but can't look up internet names (DNS)`,
				body: `Name lookups fail inside ${where}. Repairing DNS rewrites /etc/resolv.conf (the old one is backed up) and restarts nothing.`,
				primary: FIX.repair_dns,
				secondary: [FIX.restart_networking],
			};

		case 'wsl_down':
			return {
				tone: 'danger',
				title: `WSL isn't starting`,
				body: health.detail
					? `${health.detail} Restarting WSL usually clears this; it asks for administrator approval.`
					: `Restarting WSL usually clears this; it asks for administrator approval.`,
				primary: FIX.restart_networking,
				secondary: natOffer,
			};
	}
}

/** Short chip label for a state (Settings › Engines). */
export function wslHealthStateLabel(state: WslHealthState): string {
	switch (state) {
		case 'ok':
			return 'Online';
		case 'host_offline':
			return 'PC offline';
		case 'no_route':
			return 'No network';
		case 'dns_only':
			return 'DNS failing';
		case 'wsl_down':
			return 'Not starting';
		case 'not_installed':
			return 'Not installed';
	}
}
