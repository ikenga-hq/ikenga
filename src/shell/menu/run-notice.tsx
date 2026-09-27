// WP-55 — where a menu-run action's outcome goes (the review's "never drop
// the outcome"). A personal / project action run from a menu ends in a
// WP-53 `RunOutcome`; a refusal (trust, `no-target`, `permission-pending`,
// …) or a failure is shown here through the shell's `FloatingToastChip`
// (viewport-top, the same pill the notification toasts use), and a trust
// refusal also opens WP-18's trust sheet in its `project-actions` mode
// (WP-53's entry point), focused on the refused action.
//
// `MenuRunNoticeHost` is mounted once, beside the status bar (always in the
// frame). The store is module-level so `resolve.ts` can surface an outcome
// from anywhere without a React context.

import { ShieldAlert, XCircle } from 'lucide-react';
import { lazy, Suspense } from 'react';
import { create } from 'zustand';
import { FloatingToastChip } from '@/components/ui/floating-toast-chip';
import type { ConfirmRequest, RunOutcome, TrustSheetRequest } from '@/lib/actions/runner';
import { confirm as confirmDialog } from '@/lib/transport/dialog-shim';

// Loaded on first use: the sheet (react-query, the actions client) stays out
// of the status bar's module graph until a trust refusal actually opens it.
const NgwaTrustSheet = lazy(() =>
	import('@/shell/ngwa/ngwa-trust-sheet').then((m) => ({ default: m.NgwaTrustSheet }))
);

/** How long a refusal notice stays up. Long enough to read a sentence. */
export const MENU_RUN_NOTICE_TTL_MS = 6000;

export interface MenuRunNotice {
	/** Bumps on every notice so a repeat of the same text restarts the TTL. */
	seq: number;
	message: string;
	trustSheet?: TrustSheetRequest;
}

interface MenuRunNoticeState {
	notice: MenuRunNotice | null;
	trustSheet: TrustSheetRequest | null;
	show: (message: string, trustSheet?: TrustSheetRequest) => void;
	dismiss: () => void;
	openTrustSheet: (request: TrustSheetRequest) => void;
	closeTrustSheet: () => void;
}

let seq = 0;

export const useMenuRunNotice = create<MenuRunNoticeState>((set) => ({
	notice: null,
	trustSheet: null,
	show: (message, trustSheet) => set({ notice: { seq: ++seq, message, trustSheet } }),
	dismiss: () => set({ notice: null }),
	openTrustSheet: (request) => set({ trustSheet: request }),
	closeTrustSheet: () => set({ trustSheet: null }),
}));

/**
 * Surface one run outcome. `done` / `preview` are silent. `refused` shows its
 * message — except `cancelled`, which is the user's own "No" to a confirm —
 * and a trust refusal (the outcome carries a `trustSheet` request) opens the
 * trust sheet at once. `failed` shows its message.
 */
export function surfaceRunOutcome(outcome: RunOutcome): void {
	const store = useMenuRunNotice.getState();
	if (outcome.status === 'refused') {
		if (outcome.reason === 'cancelled') return;
		store.show(outcome.message, outcome.trustSheet);
		if (outcome.trustSheet) store.openTrustSheet(outcome.trustSheet);
		return;
	}
	if (outcome.status === 'failed') store.show(outcome.message);
}

/** `shell` `confirm: true` through the dialog shim (never `window.confirm`
 *  directly — the shim uses the Tauri dialog and reads a missing dialog as
 *  "not confirmed"). */
export function confirmShellRun(request: ConfirmRequest): Promise<boolean> {
	return confirmDialog(`${request.command}\n\nin ${request.cwd ?? 'your home directory'}`, {
		title: `Run “${request.name}”?`,
		kind: 'warning',
	});
}

export function MenuRunNoticeHost() {
	const notice = useMenuRunNotice((s) => s.notice);
	const trustSheet = useMenuRunNotice((s) => s.trustSheet);
	const dismiss = useMenuRunNotice((s) => s.dismiss);
	const openTrustSheet = useMenuRunNotice((s) => s.openTrustSheet);
	const closeTrustSheet = useMenuRunNotice((s) => s.closeTrustSheet);
	const noticeTrust = notice?.trustSheet;
	return (
		<>
			{notice && (
				<FloatingToastChip
					key={notice.seq}
					variant="error"
					icon={noticeTrust ? <ShieldAlert /> : <XCircle />}
					label={notice.message}
					action={noticeTrust ? { label: 'Review trust', onClick: () => openTrustSheet(noticeTrust) } : undefined}
					onDismiss={dismiss}
					ttlMs={MENU_RUN_NOTICE_TTL_MS}
				/>
			)}
			{trustSheet && (
				<Suspense fallback={null}>
					<NgwaTrustSheet
						open
						mode="project-actions"
						item={null}
						projectActions={{ projectId: trustSheet.projectId, actionIds: trustSheet.actionIds }}
						onOpenChange={(o) => {
							if (!o) closeTrustSheet();
						}}
						onApproved={closeTrustSheet}
					/>
				</Suspense>
			)}
		</>
	);
}
