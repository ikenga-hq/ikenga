// App lock screen — D-05 `locked` (`designs/people.html?state=locked`), WP-72.
//
// Mounted once per window, at the boot root (`boot/primary.tsx` and
// `boot/detached.tsx`), outside the router, so it covers every route and every
// pane. It renders into its own container on <body> and marks the app root
// `inert` while locked, so nothing underneath takes clicks, keys or focus.
//
// FOCUS (the 1Password bug, research P7-29). 1Password's lock window takes
// focus from the OS Touch ID sheet and blocks it. So this overlay never asks
// the OS for focus: no `setFocus`, no raising the window. It only moves DOM
// focus back to its own field when the window gets focus back by itself (the
// user switched back to it), and it pulls focus back if something inside the
// page tries to take it. An OS prompt, a password manager or another app can
// always take focus and keep it; when the user returns, the field has focus
// again.
//
// Unlock method: PIN or passphrase on every OS in this build. See the Rust
// header in `commands/app_lock.rs` for the per-OS biometric decision. Where
// the OS has a biometric (Windows Hello, Touch ID), D-05's second button is
// drawn but disabled, with the reason.

import { Fingerprint, ShieldCheck } from 'lucide-react';
import { type FormEvent, type RefObject, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';

import { appLockUnlock, appLockUnlockBiometric, type AppLockStatus } from '@/lib/tauri-cmd';

import { lockMetaLine, retryLine } from './app-lock-model';
import { startAppLockSync, useAppLockStore } from './app-lock-store';

/** Mount at the root of each window. Renders nothing until Rust says locked. */
export function AppLockOverlay() {
	useEffect(() => startAppLockSync(), []);
	const status = useAppLockStore((s) => s.status);
	if (!status?.locked) return null;
	return <LockedScreen status={status} />;
}

function LockedScreen({ status }: { status: AppLockStatus }) {
	const container = useLockContainer();
	const rootRef = useRef<HTMLDivElement | null>(null);
	const inputRef = useRef<HTMLInputElement | null>(null);
	const [secret, setSecret] = useState('');
	const [error, setError] = useState<string | null>(null);
	const [busy, setBusy] = useState(false);
	const [now, setNow] = useState(() => Date.now());
	const setStatus = useAppLockStore((s) => s.setStatus);

	// When the wait ends, relative to when this status arrived.
	const retryUntil = useRetryDeadline(status.retryInMs);
	const waiting = retryUntil !== null && retryUntil > now;

	useEffect(() => {
		if (retryUntil === null) return;
		const t = setInterval(() => setNow(Date.now()), 500);
		return () => clearInterval(t);
	}, [retryUntil]);

	useInertAppRoot(container !== null);
	useFocusKeeper(rootRef, inputRef, container !== null);

	if (!container) return null;

	const submit = async (e: FormEvent) => {
		e.preventDefault();
		if (busy || waiting) return;
		if (!secret) {
			setError('Enter your PIN or passphrase.');
			inputRef.current?.focus();
			return;
		}
		setBusy(true);
		try {
			const outcome = await appLockUnlock(secret);
			setSecret('');
			setStatus(outcome.status);
			if (!outcome.ok) {
				setError(outcome.error ?? 'Wrong PIN.');
				inputRef.current?.focus();
			}
		} catch (err) {
			setError(err instanceof Error ? err.message : String(err));
		} finally {
			setBusy(false);
		}
	};

	const tryBiometric = async () => {
		setBusy(true);
		try {
			const outcome = await appLockUnlockBiometric();
			setStatus(outcome.status);
			if (!outcome.ok) setError(outcome.error ?? `${status.biometric.label} didn't unlock.`);
		} catch (err) {
			setError(err instanceof Error ? err.message : String(err));
		} finally {
			setBusy(false);
		}
	};

	const errorLine = waiting && retryUntil !== null ? retryLine(retryUntil - now) : error;
	const showOs = status.biometric.kind !== 'none';
	const osReady = status.biometric.available && status.method === 'os';

	return createPortal(
		<div
			ref={rootRef}
			data-state="locked"
			role="dialog"
			aria-modal="true"
			aria-labelledby="app-lock-title"
			aria-describedby="app-lock-meta"
			className="fixed inset-0 z-[2147483000] flex flex-col items-center justify-center gap-4 overflow-auto bg-[var(--bg-base)] p-12 text-[var(--fg)]"
		>
			<h1
				id="app-lock-title"
				className="m-0 font-semibold tracking-[-0.02em] text-[var(--fg)]"
				style={{
					fontFamily: 'var(--font-display)',
					fontSize: 'var(--text-display, 40px)',
					lineHeight: 'var(--lead-display, 1.08)',
				}}
			>
				Locked
			</h1>
			<p
				id="app-lock-meta"
				className="m-0 flex items-center justify-center gap-2 font-mono text-[var(--text-micro)] text-[var(--fg-muted)]"
			>
				<ShieldCheck className="h-3.5 w-3.5 shrink-0" aria-hidden />
				<span>{lockMetaLine(status)}</span>
			</p>

			<form className="flex w-[380px] max-w-full flex-col gap-3" onSubmit={submit}>
				<span
					className={
						'flex h-[var(--btn-h-lg,40px)] items-center rounded-[var(--radius-sm)] border bg-[var(--bg-sunken)] px-3 ' +
						(error && !waiting
							? 'border-[var(--danger)]'
							: 'border-[var(--border)] focus-within:border-[var(--primary)]')
					}
				>
					<input
						ref={inputRef}
						type="password"
						autoComplete="current-password"
						spellCheck={false}
						value={secret}
						disabled={waiting}
						onChange={(e) => {
							setSecret(e.target.value);
							setError(null);
						}}
						placeholder="PIN or passphrase"
						aria-label="PIN or passphrase"
						aria-invalid={Boolean(error) || undefined}
						aria-describedby="app-lock-error"
						className="h-full w-full bg-transparent font-mono text-[var(--text-body-sm,13px)] text-[var(--fg)] outline-none placeholder:text-[var(--fg-faint)] disabled:opacity-50"
					/>
				</span>
				<div
					id="app-lock-error"
					role="alert"
					className="min-h-4 text-[var(--text-micro)] text-[var(--danger)]"
				>
					{errorLine}
				</div>
				<button
					type="submit"
					disabled={busy || waiting}
					className="flex h-[var(--btn-h-lg,40px)] items-center justify-center rounded-[var(--radius-sm)] bg-[var(--primary)] px-4 text-[var(--text-caption,12px)] font-medium text-[var(--primary-fg)] outline-none hover:opacity-90 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--primary)] disabled:opacity-50"
				>
					{busy ? 'Checking…' : 'Unlock'}
				</button>
				{showOs && (
					<button
						type="button"
						disabled={!osReady || busy}
						onClick={() => void tryBiometric()}
						title={status.biometric.available ? undefined : status.biometric.reason}
						aria-describedby={status.biometric.available ? undefined : 'app-lock-os-why'}
						className="flex h-[var(--btn-h-lg,40px)] items-center justify-center gap-2 rounded-[var(--radius-sm)] border border-[var(--border)] bg-[var(--bg-surface)] px-4 text-[var(--text-caption,12px)] text-[var(--fg)] outline-none hover:bg-[var(--bg-raised)] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--primary)] disabled:cursor-not-allowed disabled:opacity-45"
					>
						<Fingerprint className="h-4 w-4 shrink-0" aria-hidden />
						Use {status.biometric.label}
					</button>
				)}
				{showOs && !status.biometric.available && (
					<span id="app-lock-os-why" className="sr-only">
						{status.biometric.reason}
					</span>
				)}
			</form>

			<p className="m-0 max-w-[52ch] text-center text-[var(--text-micro)] leading-relaxed text-[var(--fg-muted)]">
				<b className="font-medium text-[var(--fg)]">Sessions and runs keep going</b> underneath.
				Locking hides the window; it does not stop Chi, and it does not lock the vault — that has
				its own lock in Settings › Secrets.
			</p>
			{status.configPath && (
				<p className="m-0 max-w-[60ch] text-center text-[10px] leading-relaxed text-[var(--fg-faint)]">
					Forgot it? Quit Ikenga and delete{' '}
					<span className="font-mono break-all">{status.configPath}</span>. This lock is a privacy
					screen, not a security boundary.
				</p>
			)}
		</div>,
		container
	);
}

/** A dedicated container on <body>, outside the (inert) app root. */
function useLockContainer(): HTMLElement | null {
	const [el, setEl] = useState<HTMLElement | null>(null);
	useLayoutEffect(() => {
		const node = document.createElement('div');
		node.setAttribute('data-app-lock', '');
		document.body.appendChild(node);
		setEl(node);
		return () => {
			node.remove();
		};
	}, []);
	return el;
}

/** While locked, the app root takes no input, focus or screen-reader reading. */
function useInertAppRoot(active: boolean) {
	useLayoutEffect(() => {
		if (!active) return;
		const root = document.getElementById('root');
		if (!root) return;
		const hadInert = root.hasAttribute('inert');
		const hadHidden = root.getAttribute('aria-hidden');
		root.setAttribute('inert', '');
		root.setAttribute('aria-hidden', 'true');
		return () => {
			if (!hadInert) root.removeAttribute('inert');
			if (hadHidden === null) root.removeAttribute('aria-hidden');
			else root.setAttribute('aria-hidden', hadHidden);
		};
	}, [active]);
}

/**
 * Keep focus and keys on the lock without ever asking the OS for focus.
 *
 * - The field is focused when the lock mounts, and again whenever the window
 *   gets focus back by itself (`window` `focus`). The OS decides when that
 *   happens, which is what lets a Windows Hello / Touch ID / password-manager
 *   prompt keep focus while it's up.
 * - A `focusin` outside the lock (something in the page taking focus) is
 *   sent back to the field.
 * - Keys aimed outside the lock are dropped. Keys inside it stop at
 *   <document>, so the window-level key dispatcher (⌘K, pane keys, …) never
 *   sees them.
 */
function useFocusKeeper(
	rootRef: RefObject<HTMLDivElement | null>,
	inputRef: RefObject<HTMLInputElement | null>,
	active: boolean
) {
	useEffect(() => {
		if (!active) return;
		const inside = (node: EventTarget | null) =>
			node instanceof Node && rootRef.current !== null && rootRef.current.contains(node);
		const focusField = () => {
			const input = inputRef.current;
			if (input && !input.disabled) input.focus({ preventScroll: true });
			else rootRef.current?.querySelector<HTMLElement>('button:not([disabled])')?.focus();
		};

		const raf = requestAnimationFrame(focusField);
		const onWindowFocus = () => focusField();
		const onFocusIn = (e: FocusEvent) => {
			if (!inside(e.target)) focusField();
		};
		const onKeyCapture = (e: KeyboardEvent) => {
			if (inside(e.target)) return;
			e.preventDefault();
			e.stopImmediatePropagation();
			focusField();
		};
		const onKeyBubble = (e: KeyboardEvent) => {
			if (inside(e.target)) e.stopPropagation();
		};

		window.addEventListener('focus', onWindowFocus);
		document.addEventListener('focusin', onFocusIn, true);
		window.addEventListener('keydown', onKeyCapture, true);
		document.addEventListener('keydown', onKeyBubble);
		return () => {
			cancelAnimationFrame(raf);
			window.removeEventListener('focus', onWindowFocus);
			document.removeEventListener('focusin', onFocusIn, true);
			window.removeEventListener('keydown', onKeyCapture, true);
			document.removeEventListener('keydown', onKeyBubble);
		};
	}, [active, rootRef, inputRef]);
}

/** Turn a relative `retryInMs` into an absolute deadline, once per status. */
function useRetryDeadline(retryInMs: number | null): number | null {
	const [deadline, setDeadline] = useState<number | null>(() =>
		retryInMs ? Date.now() + retryInMs : null
	);
	useEffect(() => {
		setDeadline(retryInMs ? Date.now() + retryInMs : null);
	}, [retryInMs]);
	return deadline;
}
