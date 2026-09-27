// The one key dispatcher (WP-54, DEC-56) — the registry is the single firing
// path for every frame key.
//
// One window `keydown` listener resolves key → chord → `when` → command over
// the effective keymap (`getKeymap()`, G-ACTIONS §2.2) and runs the command
// through the command table (`commands.ts`):
//
//   1. IME composition, Dead keys and bare modifiers never match
//      (`strokesFromEvent`, §3.3). An event a widget already consumed
//      (`defaultPrevented`) is left alone unless a chord is pending.
//   2. **Hosted** commands (§4.6) are never fired here. When a hosted rule
//      whose owner has focus matches (`terminal.*` with `terminalFocus`, the
//      Companion dispatch keys with `dispatchFocus`), the keystroke is the
//      owner's: the dispatcher neither fires nor `preventDefault`s it.
//   3. Chord mode (DEC-57, §3.2) through WP-49's `ChordMachine`: a stroke is
//      held back for 900 ms only while the effective keymap binds a chord
//      starting with it (with the owner carve-out while the terminal, the
//      dispatch input or the palette has focus). With no ⌘K chord bound,
//      ⌘K resolves at once — the palette opens with no delay.
//   4. The single-stroke winner is G-ACTIONS-API `resolveKeypress()` (§2.3:
//      layer, then `when` specificity, then merge order). Exactly one command
//      fires (DEC-58). A `!inputFocus` rule never fires while typing; only a
//      rule whose `when` holds there (`always`, `paletteOpen`, …) does.
//   5. The winner runs only when this window can run it (a registered
//      handler, or the effective-action fallback); otherwise the keystroke
//      keeps its default. On macOS the run is deduplicated against the
//      native-menu accelerator for the same command (`claimSingleFire`).
//
// OS-wide rules (`scope: 'os'`, §6) are not dispatched here: the primary
// window pushes the effective OS rules to `lib.rs` (`startOsShortcutSync`),
// which (re-)registers them with the OS and relays a non-`os.*` command back
// as `keymap://os-command`.

import { useEffect, useRef } from 'react';
import { listen, osShortcutsApply } from '@/lib/tauri-cmd';
import { type ChordBinding, ChordMachine, chordPrefixFor } from './chord';
import {
	type CommandHandler,
	type CommandInvocation,
	type CommandSource,
	canRunCommand,
	claimSingleFire,
	registerCommand,
	runCommand,
} from './commands';
import { type ContextKeys, getContextKeys, getEvalOptions } from './context-keys';
import type { KeymapEntry } from './defaults';
import { eventMatchesCombo, isChordSequence, isMacPlatform, strokesFromEvent } from './platform';
import {
	entriesForPlatform,
	getKeymap,
	isHostedCommand,
	type KeymapPlatform,
	resolveKeypress,
	resolveKeypressWinner,
	subscribeKeymap,
} from './registry';
import { type EvalOptions, evaluateWhen } from './when';

// ─── Hosted owners (§4.6) ─────────────────────────────────────────────────

/** A hosted command's owner widget. */
export type HostedOwner = 'terminal' | 'dispatch';

const DISPATCH_COMMANDS: ReadonlySet<string> = new Set([
	'companion.send',
	'companion.new-run',
	'companion.persistent-run',
]);

function ownsCommand(owner: HostedOwner | undefined, command: string): boolean {
	if (owner === 'terminal') return command.startsWith('terminal.');
	if (owner === 'dispatch') return DISPATCH_COMMANDS.has(command);
	return isHostedCommand(command);
}

// ─── The dispatcher ───────────────────────────────────────────────────────

export interface KeyDispatcherOptions {
	/** The effective keymap (default: `getKeymap()`, read on every press so a
	 *  re-merge takes effect without a remount). */
	getEntries?: () => readonly KeymapEntry[];
	platform?: () => KeymapPlatform;
	/** Context keys for a keydown's target (default: the live service). */
	getContext?: (target: EventTarget | null) => ContextKeys;
	getEvalOptions?: () => EvalOptions;
	/** Runs a command (default: the command table). True = it ran. */
	run?: (invocation: CommandInvocation) => boolean;
	/** Can this window run `command`? (default: the command table). */
	canRun?: (command: string) => boolean;
	/** DEC-58 single fire (default: `claimSingleFire(command, 'dispatcher')`). */
	claim?: (command: string) => boolean;
	timeoutMs?: number;
	setTimer?: (fn: () => void, ms: number) => unknown;
	clearTimer?: (handle: unknown) => void;
}

/** What the dispatcher would do with a keydown, without doing it — the
 *  xterm hook asks before letting a key reach the PTY. */
export interface KeyPeek {
	/** The dispatcher would act on this key (fire, or hold it for a chord). */
	claimed: boolean;
	/** The single-stroke winner, when that is what would fire. */
	winner: KeymapEntry | null;
	/** The key is (or completes) a chord stroke. */
	chord: boolean;
}

const NO_PEEK: KeyPeek = { claimed: false, winner: null, chord: false };

export class KeyDispatcher {
	private readonly machine: ChordMachine<KeymapEntry>;
	private readonly entries: () => readonly KeymapEntry[];
	private readonly platform: () => KeymapPlatform;
	private readonly context: (target: EventTarget | null) => ContextKeys;
	private readonly evalOptions: () => EvalOptions;
	private readonly runFn: (invocation: CommandInvocation) => boolean;
	private readonly canRun: (command: string) => boolean;
	private readonly claim: (command: string) => boolean;

	constructor(opts: KeyDispatcherOptions = {}) {
		this.entries = opts.getEntries ?? getKeymap;
		this.platform = opts.platform ?? (() => (isMacPlatform() ? 'mac' : 'other'));
		this.context = opts.getContext ?? ((target) => getContextKeys(target));
		this.evalOptions = opts.getEvalOptions ?? getEvalOptions;
		this.runFn = opts.run ?? runCommand;
		this.canRun = opts.canRun ?? canRunCommand;
		this.claim = opts.claim ?? ((command) => claimSingleFire(command, 'dispatcher'));
		this.machine = new ChordMachine<KeymapEntry>({
			getBindings: () => this.chordBindings(),
			mac: () => this.platform() === 'mac',
			onTimeout: (outcome) => {
				// 900 ms passed: the first stroke's single-stroke winner, as
				// resolved at the first keypress (§3.2), fires.
				if (outcome.winner) this.fire(outcome.winner, 'key');
			},
			timeoutMs: opts.timeoutMs,
			setTimer: opts.setTimer,
			clearTimer: opts.clearTimer,
		});
	}

	/** True while a chord's first stroke waits for its second. */
	get chordPending(): boolean {
		return this.machine.isPending;
	}

	/** The pending first stroke (resolved form), for a status hint. */
	get pendingPrefix(): string | null {
		return this.machine.pendingPrefix;
	}

	/** Leave chord mode without firing (window blur). */
	cancelChord(): void {
		this.machine.cancel();
	}

	/** In-app, non-hosted chords on this platform — what chord mode reads. */
	private chordBindings(): ChordBinding[] {
		return entriesForPlatform(this.entries(), this.platform()).filter(
			(e) => (e.scope ?? 'app') === 'app' && !isHostedCommand(e.command) && isChordSequence(e.key)
		);
	}

	/**
	 * The hosted rule (§4.6) that claims `e` now, or null: a hosted command
	 * of `owner` (default: any owner) whose key matches and whose `when` —
	 * its owner's focus key — is true against `ctx`. §2.3 ranks several.
	 */
	hostedWinner(e: KeyboardEvent, ctx: ContextKeys, owner?: HostedOwner): KeymapEntry | null {
		const platform = this.platform();
		const mac = platform === 'mac';
		const entries = this.entries();
		const evalOpts = this.evalOptions();
		const hits = entriesForPlatform(entries, platform).filter(
			(entry) =>
				(entry.scope ?? 'app') === 'app' &&
				isHostedCommand(entry.command) &&
				ownsCommand(owner, entry.command) &&
				eventMatchesCombo(e, entry.key, mac) &&
				evaluateWhen(entry.when, ctx, evalOpts)
		);
		return hits.length > 0 ? resolveKeypressWinner(hits, entries) : null;
	}

	/** `hostedWinner` against the live context of the event's target. */
	resolveHosted(e: KeyboardEvent, owner: HostedOwner): KeymapEntry | null {
		if (strokesFromEvent(e).length === 0) return null;
		return this.hostedWinner(e, this.context(e.target), owner);
	}

	/** What `handleKeydown(e)` would do, with no side effects. */
	peek(e: KeyboardEvent): KeyPeek {
		if (this.machine.isPending) return { claimed: true, winner: null, chord: true };
		const strokes = strokesFromEvent(e);
		if (strokes.length === 0) return NO_PEEK;
		const ctx = this.context(e.target);
		if (this.hostedWinner(e, ctx)) return NO_PEEK;
		const platform = this.platform();
		const prefix = chordPrefixFor(strokes, this.chordBindings(), {
			mac: platform === 'mac',
			ctx,
			evalOpts: this.evalOptions(),
		});
		if (prefix !== null) return { claimed: true, winner: null, chord: true };
		const winner = resolveKeypress(e, ctx, platform, this.entries()).winner;
		return { claimed: winner !== null && this.canRun(winner.command), winner, chord: false };
	}

	/**
	 * Handle one keydown. Returns true when the dispatcher acted on it (fired
	 * a command, held it for a chord, or ended a chord) — it has then called
	 * `preventDefault()`.
	 */
	handleKeydown(e: KeyboardEvent): boolean {
		const pending = this.machine.isPending;
		if (e.defaultPrevented && !pending) return false;
		const strokes = strokesFromEvent(e);
		if (strokes.length === 0 && !pending) return false;
		const ctx = this.context(e.target);
		// §4.6: a hosted rule whose owner has focus owns this keystroke.
		if (!pending && this.hostedWinner(e, ctx)) return false;

		const entries = this.entries();
		const platform = this.platform();
		const single = strokes.length > 0 ? resolveKeypress(e, ctx, platform, entries).winner : null;
		const outcome = this.machine.press(strokes, ctx, single ?? undefined, this.evalOptions());

		switch (outcome.type) {
			case 'none':
				return single ? this.fire(single, 'key', e) : false;
			case 'pending':
			case 'cancelled':
				e.preventDefault();
				return true;
			case 'chord': {
				e.preventDefault();
				const winner = resolveKeypressWinner(outcome.candidates as KeymapEntry[], entries);
				if (winner) this.fire(winner, 'chord', e);
				return true;
			}
			case 'fallthrough':
				// The second stroke completed no chord: the first stroke's
				// winner fires, then this stroke is dispatched afresh.
				if (outcome.winner) this.fire(outcome.winner, 'key');
				this.handleKeydown(e);
				return true;
			default:
				return false;
		}
	}

	private fire(entry: KeymapEntry, source: CommandSource, e?: KeyboardEvent): boolean {
		if (!this.canRun(entry.command)) return false;
		e?.preventDefault();
		// DEC-58: the native-menu accelerator already ran it for this press.
		if (!this.claim(entry.command)) return true;
		this.runFn({ command: entry.command, source, event: e });
		return true;
	}
}

// ─── The window's instance ────────────────────────────────────────────────

let instance: KeyDispatcher | null = null;
let teardown: (() => void) | null = null;

/** This window's dispatcher. */
export function getKeyDispatcher(): KeyDispatcher {
	instance ??= new KeyDispatcher();
	return instance;
}

/**
 * Install the one window listener (idempotent; every window calls it at
 * boot, and `useCommands` makes sure it is there). Returns the teardown.
 */
export function installKeyDispatcher(): () => void {
	if (teardown) return teardown;
	if (typeof window === 'undefined') return () => {};
	const dispatcher = getKeyDispatcher();
	const onKey = (e: KeyboardEvent) => {
		dispatcher.handleKeydown(e);
	};
	const onBlur = () => dispatcher.cancelChord();
	window.addEventListener('keydown', onKey);
	window.addEventListener('blur', onBlur);
	teardown = () => {
		window.removeEventListener('keydown', onKey);
		window.removeEventListener('blur', onBlur);
		dispatcher.cancelChord();
		teardown = null;
	};
	return teardown;
}

/** The hosted rule of `owner` that claims `e` (the xterm hook and the
 *  dispatch input fire their commands from this, §4.6). */
export function resolveHostedKeypress(e: KeyboardEvent, owner: HostedOwner): KeymapEntry | null {
	return getKeyDispatcher().resolveHosted(e, owner);
}

/** `getKeyDispatcher().peek(e)`. */
export function peekKeypress(e: KeyboardEvent): KeyPeek {
	return getKeyDispatcher().peek(e);
}

// ─── React: register handlers ─────────────────────────────────────────────

/**
 * Register a set of command handlers while the calling component is mounted
 * (and installs the dispatcher if nothing has yet). Handlers are read
 * through a ref, so passing fresh closures every render never re-registers;
 * only a change to the set of command ids does.
 */
export function useCommands(
	handlers: Readonly<Record<string, CommandHandler>>,
	opts?: { enabled?: boolean }
): void {
	const ref = useRef(handlers);
	ref.current = handlers;
	const enabled = opts?.enabled ?? true;
	const ids = Object.keys(handlers).sort().join('\n');
	useEffect(() => {
		if (!enabled || !ids) return;
		installKeyDispatcher();
		const offs = ids
			.split('\n')
			.map((command) => registerCommand(command, (invocation) => ref.current[command]?.(invocation)));
		return () => {
			for (const off of offs) off();
		};
	}, [ids, enabled]);
}

/** One command's handler while mounted — `useCommands({ [command]: handler })`. */
export function useCommand(command: string, handler: CommandHandler, opts?: { enabled?: boolean }): void {
	useCommands({ [command]: handler }, opts);
}

// ─── OS-wide rules (§6, DEC-60) ───────────────────────────────────────────

export interface OsShortcutRule {
	command: string;
	key: string;
}

/** Per-rule registration result from `lib.rs` — the Keys tab shows a failed
 *  one as "not registered: <reason>" (§6). */
export interface OsShortcutStatus {
	command: string;
	key: string;
	registered: boolean;
	reason: string | null;
}

/**
 * The effective OS rules on `platform`: `scope: 'os'` entries (default +
 * personal — the merge already dropped project OS rules, `E_OS_LAYER`),
 * single strokes only, never a package id or a hosted command (§6).
 */
export function osRulesFor(entries: readonly KeymapEntry[], platform: KeymapPlatform): OsShortcutRule[] {
	return entriesForPlatform(entries, platform)
		.filter(
			(e) =>
				e.scope === 'os' &&
				e.source !== 'project' &&
				e.source !== 'package' &&
				!e.command.includes(':') &&
				!isHostedCommand(e.command) &&
				!isChordSequence(e.key)
		)
		.map((e) => ({ command: e.command, key: e.key }));
}

let osStatuses: OsShortcutStatus[] = [];
const osStatusListeners = new Set<() => void>();

export function getOsShortcutStatuses(): OsShortcutStatus[] {
	return osStatuses;
}

export function subscribeOsShortcutStatuses(listener: () => void): () => void {
	osStatusListeners.add(listener);
	return () => {
		osStatusListeners.delete(listener);
	};
}

function setOsStatuses(next: OsShortcutStatus[]): void {
	osStatuses = next;
	for (const listener of [...osStatusListeners]) listener();
}

/** Payload of `keymap://os-command` — an OS rule bound to a non-`os.*`
 *  command fired while Ikenga may be unfocused (§6). */
export const OS_COMMAND_EVENT = 'keymap://os-command';

export interface OsShortcutSyncDeps {
	apply?: (rules: OsShortcutRule[]) => Promise<OsShortcutStatus[]>;
	getEntries?: () => readonly KeymapEntry[];
	platform?: () => KeymapPlatform;
	subscribe?: (listener: () => void) => () => void;
	listenOsCommand?: (handler: (command: string) => void) => Promise<() => void>;
}

/**
 * Primary window only: push the effective OS rules to `lib.rs` now and on
 * every keymap publish whose OS rules changed (a personal rebind re-registers
 * with the OS), and run commands the OS relays back. Returns the teardown.
 */
export function startOsShortcutSync(deps: OsShortcutSyncDeps = {}): () => void {
	const apply: (rules: OsShortcutRule[]) => Promise<OsShortcutStatus[]> = deps.apply ?? osShortcutsApply;
	const entries: () => readonly KeymapEntry[] = deps.getEntries ?? getKeymap;
	const platform: () => KeymapPlatform = deps.platform ?? (() => (isMacPlatform() ? 'mac' : 'other'));
	const subscribe: (listener: () => void) => () => void = deps.subscribe ?? subscribeKeymap;
	const listenOsCommand: (handler: (command: string) => void) => Promise<() => void> =
		deps.listenOsCommand ??
		((handler: (command: string) => void) =>
			listen<{ command?: string }>(OS_COMMAND_EVENT, (event) => {
				const command = event.payload?.command;
				if (typeof command === 'string') handler(command);
			}));

	let last: string | null = null;
	let disposed = false;
	const push = () => {
		const rules = osRulesFor(entries(), platform());
		const signature = JSON.stringify(rules);
		if (signature === last) return;
		last = signature;
		apply(rules)
			.then((statuses) => {
				if (disposed) return;
				setOsStatuses(statuses);
				for (const s of statuses) {
					if (!s.registered) console.warn(`[keymap] OS shortcut ${s.key} → ${s.command} not registered: ${s.reason}`);
				}
			})
			.catch((err) => {
				// A failed push must not stick: the next publish retries.
				last = null;
				console.warn('[keymap] could not register OS shortcuts:', err);
			});
	};
	push();
	const unsubscribe = subscribe(push);

	let unlisten: (() => void) | null = null;
	listenOsCommand((command) => {
		runCommand({ command, source: 'os' });
	})
		.then((un) => {
			if (disposed) un();
			else unlisten = un;
		})
		.catch(() => {
			// Non-Tauri context: no OS shortcuts to relay.
		});

	return () => {
		disposed = true;
		unsubscribe();
		unlisten?.();
	};
}
