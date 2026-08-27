/**
 * pty-bridge — typed orchestration around the Rust pty_* commands.
 *
 * Exposes a small `Pty` class that owns the lifecycle of a single PTY:
 *   - subscribes to the data + exit event streams
 *   - fans out events to multiple subscribers (so the same PTY can be
 *     attached/detached from multiple xterm hosts during its life)
 *   - tracks exit state so late-attaching hosts get the right status
 */

import {
	ptyAttachArm,
	ptyAttachBegin,
	ptyKill,
	ptyListen,
	ptyResize,
	ptySpawn,
	ptyWrite,
	type PtySpawnOpts as RawPtySpawnOpts,
} from '../lib/tauri-cmd';

export interface PtySpawnOpts extends RawPtySpawnOpts {
	/** Human-readable label, e.g. `bash -l`. Used for status messages. */
	label?: string;
}

export type PtyDataHandler = (bytes: Uint8Array) => void;
export type PtyExitHandler = (code: number | null) => void;

/**
 * Re-exports of the raw command wrappers, in case anyone wants the imperative
 * API without the class.
 */
export { ptyAttachArm, ptyAttachBegin, ptyKill, ptyListen, ptyResize, ptySpawn, ptyWrite };

export class Pty {
	readonly id: string;
	readonly label: string;
	cwd?: string;
	exited: boolean = false;
	exitCode: number | null = null;

	private dataSubs = new Set<PtyDataHandler>();
	private exitSubs = new Set<PtyExitHandler>();
	private unlisten: (() => void) | null = null;
	private disposed = false;
	/**
	 * Owning PTYs (created via `spawn`) kill the core PTY on `dispose`. Attached
	 * PTYs (created via `attach`, e.g. a terminal popped out into a detached
	 * window) only unsubscribe — the origin pane still owns + kills the PTY.
	 */
	private owning = true;
	/**
	 * Bytes received before any subscriber registered. Held so the first
	 * `onData()` consumer can catch up on everything the PTY has emitted so
	 * far — without this, the spawn-before-mount race leaves xterm blank.
	 *
	 * Retention rule (see `onData` / `deliverData`): subscribing REPLAYS the
	 * buffer but does not destroy it. It is released only once a live chunk
	 * actually reaches an attached subscriber — the earliest point at which a
	 * renderer has demonstrably survived long enough to own the scrollback from
	 * here on. Draining on mere subscription lost the backlog to any consumer
	 * that attached and detached without rendering (React StrictMode's
	 * double-mount, a deferred mount), which is what left a popped-out idle
	 * terminal blank.
	 */
	private replayBuffer: Uint8Array = new Uint8Array(0);
	/**
	 * Absolute stream offset of `replayBuffer[0]` (cumulative bytes emitted
	 * before it), or `null` when the buffer is empty. Reset to `null` whenever
	 * the buffer drains. Read by `primeExternalSnapshot` to work out how much of
	 * a pending backlog an externally-painted snapshot already covers.
	 */
	private replayStartOffset: number | null = null;
	/**
	 * When set, drop any live bytes whose absolute offset is below this value —
	 * they were already painted by an EXTERNAL snapshot (the per-session capture
	 * ring; see `primeExternalSnapshot`). Cleared as soon as a live chunk crosses
	 * the threshold (offsets are monotonic).
	 *
	 * The detached-attach path does NOT use this: `Pty.attach` gets its snapshot
	 * and its live subscription atomically from Rust, so the two never overlap.
	 */
	private dedupUpTo: number | null = null;
	/**
	 * Absolute stream offset of the last byte delivered by the underlying
	 * `pty://<id>` listener — i.e. the cumulative count of bytes this PTY has
	 * emitted so far. Updated on every `deliverData` (including buffered and
	 * deduped chunks, so it always reflects the true stream position). Read by
	 * the per-session capture ring (`pty-output-buffer.ts`) so its snapshot can
	 * be tagged with an absolute offset and reconciled against this stream (see
	 * `primeExternalSnapshot`).
	 */
	private totalOffset = 0;

	private constructor(id: string, label: string, cwd?: string) {
		this.id = id;
		this.label = label;
		this.cwd = cwd;
	}

	setCwd(cwd: string): void {
		this.cwd = cwd;
	}

	/**
	 * Cumulative bytes this PTY has emitted (absolute stream offset of the
	 * most recent byte). Lets an external snapshot buffer (the capture ring)
	 * tag its contents with an absolute end offset comparable to this stream's
	 * offsets, so the seam between the two can be deduped rather than
	 * double-painted.
	 */
	get streamOffset(): number {
		return this.totalOffset;
	}

	/**
	 * Internal fanout. `endOffset` is the cumulative byte count this chunk ends
	 * at (its absolute start is `endOffset - bytes.length`). If no subscriber
	 * has registered yet, the bytes are appended to `replayBuffer` so the
	 * eventual `onData()` consumer can catch up; once a subscriber exists, bytes
	 * flow live. `dedupUpTo` trims bytes already painted by an external snapshot
	 * (the capture ring — see `primeExternalSnapshot`).
	 */
	private deliverData(bytes: Uint8Array, endOffset: number) {
		// Track the absolute stream position first — before any dedup/return —
		// so `streamOffset` stays accurate even for chunks that get dropped.
		this.totalOffset = endOffset;
		if (this.dedupUpTo !== null) {
			const start = endOffset - bytes.length;
			if (endOffset <= this.dedupUpTo) {
				// Wholly inside the replayed snapshot — drop it. Clear the
				// threshold once we're exactly at its edge (next chunk is above).
				if (endOffset === this.dedupUpTo) this.dedupUpTo = null;
				return;
			}
			if (start < this.dedupUpTo) {
				bytes = bytes.subarray(this.dedupUpTo - start);
			}
			// This chunk crosses the threshold; all later ones are above it.
			this.dedupUpTo = null;
		}
		if (this.dataSubs.size === 0) {
			if (this.replayStartOffset === null) {
				this.replayStartOffset = endOffset - bytes.length;
			}
			const next = new Uint8Array(this.replayBuffer.length + bytes.length);
			next.set(this.replayBuffer, 0);
			next.set(bytes, this.replayBuffer.length);
			this.replayBuffer = next;
			return;
		}
		// A live chunk is about to reach an attached subscriber. That subscriber
		// has now outlived its own mount long enough to receive real output, so
		// the pending backlog has demonstrably landed in a renderer that is
		// still there — release it. Doing this here rather than in `onData` is
		// what lets a subscriber attach, replay, and detach without consuming
		// the backlog (StrictMode double-mount / deferred mount), while still
		// guaranteeing the backlog is not re-replayed once a renderer is
		// genuinely live on the stream.
		if (this.replayBuffer.length > 0) {
			this.replayBuffer = new Uint8Array(0);
			this.replayStartOffset = null;
		}
		for (const sub of this.dataSubs) {
			try {
				sub(bytes);
			} catch (err) {
				console.error('[pty] data handler threw', err);
			}
		}
	}

	/**
	 * Put a scrollback snapshot in front of the pending backlog, so a detached
	 * (non-owning) terminal replays recent scrollback before the live stream.
	 *
	 * A plain prepend — no offset reconciliation — because `attach` obtains the
	 * snapshot and its live subscription atomically: Rust gates the stream at
	 * the instant it snapshots, and holds every byte emitted during the
	 * handshake until the listener is registered, then flushes them as the first
	 * live chunk. The snapshot ends at `snapEnd` and the live stream resumes at
	 * `snapEnd`, so there is no overlap to trim and no gap to fill.
	 *
	 * `snapEnd` is only used to keep `replayStartOffset` truthful (the absolute
	 * offset of `replayBuffer[0]`).
	 */
	private prependScrollback(snapData: Uint8Array, snapEnd: number) {
		if (snapData.length === 0) return;
		const merged = new Uint8Array(snapData.length + this.replayBuffer.length);
		merged.set(snapData, 0);
		merged.set(this.replayBuffer, snapData.length);
		this.replayBuffer = merged;
		this.replayStartOffset = snapEnd - snapData.length;
	}

	/**
	 * Reconcile an EXTERNAL snapshot the caller has already painted (e.g. the
	 * per-session capture ring replayed straight into xterm) against this
	 * PTY's stream. `snapEnd` is the absolute offset the caller painted up to
	 * (`streamOffset` at snapshot time). Any bytes at/below `snapEnd` — whether
	 * already buffered in `replayBuffer` for a not-yet-attached subscriber or
	 * arriving live afterwards — are dropped, so the seam is not double-painted.
	 *
	 * This is the one remaining consumer of `dedupUpTo` / `replayStartOffset`,
	 * and it still needs them: unlike `attach`, its snapshot is produced by a
	 * separate JS-side mechanism (the capture ring, tagged with `streamOffset`)
	 * that the Rust attach gate knows nothing about, so its seam is reconciled
	 * by offset rather than closed by construction. Must be called BEFORE the
	 * consuming `onData()` subscriber
	 * attaches (so the buffered replay is trimmed before it is drained). No-op
	 * on an empty snapshot. Offsets are monotonic.
	 */
	primeExternalSnapshot(snapEnd: number) {
		if (snapEnd <= 0) return;
		if (this.replayStartOffset !== null) {
			const start = this.replayStartOffset;
			const end = start + this.replayBuffer.length;
			if (snapEnd >= end) {
				// Whole buffer already covered by the external snapshot.
				this.replayBuffer = new Uint8Array(0);
				this.replayStartOffset = null;
			} else if (snapEnd > start) {
				// Drop the covered prefix; keep the tail beyond the seam.
				this.replayBuffer = this.replayBuffer.subarray(snapEnd - start);
				this.replayStartOffset = snapEnd;
			}
			// else snapEnd <= start: buffer is wholly after the seam — keep it.
		}
		// Suppress future live bytes below the seam (self-clears once the
		// stream crosses `snapEnd`, per `deliverData`).
		this.dedupUpTo = snapEnd;
	}

	/**
	 * Spawn a new PTY and start listening on its event streams.
	 */
	static async spawn(opts: PtySpawnOpts): Promise<Pty> {
		const id = await ptySpawn({
			// `terminalId` + `title` are what let the Rust core name this PTY in
			// `TerminalDescriptor` — the view agents read over
			// `/iyke/terminal/list` and the shell reads back for tab titles.
			// Forwarding them is not optional: dropping them leaves every
			// terminal anonymous on that surface.
			terminalId: opts.terminalId,
			title: opts.title,
			cwd: opts.cwd,
			cmd: opts.cmd,
			env: opts.env,
			rows: opts.rows,
			cols: opts.cols,
		});
		const pty = new Pty(id, opts.label ?? opts.cmd.join(' '), opts.cwd);
		try {
			pty.unlisten = await ptyListen(
				id,
				(bytes, endOffset) => pty.deliverData(bytes, endOffset),
				(code) => {
					pty.exited = true;
					pty.exitCode = code;
					for (const sub of pty.exitSubs) {
						try {
							sub(code);
						} catch (err) {
							console.error('[pty] exit handler threw', err);
						}
					}
				}
			);
		} catch (err) {
			// If we failed to subscribe, kill the orphan to avoid leaks.
			await ptyKill(id).catch(() => {});
			throw err;
		}
		return pty;
	}

	/**
	 * Attach to an EXISTING PTY by id (a terminal popped out into a detached
	 * window). Subscribes to the live `pty://<id>` stream without spawning —
	 * the origin pane still owns the PTY, so `dispose()` here only unsubscribes
	 * and never kills it. New output + keystrokes flow live and write back to
	 * the shared shell (both windows drive the same PTY).
	 *
	 * Scrollback: a three-step atomic handshake, NOT a listen-then-fetch race.
	 *
	 *   1. `ptyAttachBegin` — Rust snapshots the scrollback ring and gates the
	 *      stream under the same lock the emitter holds. From here the PTY
	 *      delivers nothing to anyone.
	 *   2. `ptyListen` — register the live subscription inside that quiet window.
	 *   3. `ptyAttachArm` — release the gate; everything emitted during the
	 *      handshake arrives as the first live chunk, starting exactly where the
	 *      snapshot ended.
	 *
	 * Because there is no interval in which a byte is both snapshotted and
	 * delivered live, nothing is duplicated and nothing is dropped — which is
	 * why the old `dedupUpTo` / offset-merge reconciliation is gone.
	 *
	 * The replayed bytes are still the raw trailing stream, so — like any
	 * terminal reattach — mid-state escape sequences render against a fresh
	 * screen; a few stale bytes may flicker at the top on first paint (that is
	 * the separate T-3 paint bug, not a seam bug). Scrollback older than the
	 * Rust ring cap (256KB) is not replayed.
	 */
	static async attach(id: string, label: string): Promise<Pty> {
		const pty = new Pty(id, label);
		pty.owning = false;

		// 1. Snapshot + gate. A failure here (or a reaped session) just means no
		//    scrollback replay; the live attach below still works.
		let snap: Awaited<ReturnType<typeof ptyAttachBegin>> = null;
		try {
			snap = await ptyAttachBegin(id);
		} catch (err) {
			console.warn('[pty] attach snapshot failed', err);
		}

		try {
			// 2. Subscribe while the stream is quiet.
			pty.unlisten = await ptyListen(
				id,
				(bytes, endOffset) => pty.deliverData(bytes, endOffset),
				(code) => {
					pty.exited = true;
					pty.exitCode = code;
					for (const sub of pty.exitSubs) {
						try {
							sub(code);
						} catch (err) {
							console.error('[pty] exit handler threw', err);
						}
					}
				}
			);
		} finally {
			// 3. Release the gate — in a `finally` so a failed subscription can
			//    never leave the terminal stalled waiting on the watchdog.
			if (snap) {
				// Prepend before arming: the flush must land *after* the
				// snapshot in `replayBuffer`, and it cannot arrive before the
				// arm call is made.
				pty.prependScrollback(snap.data, snap.endOffset);
				try {
					await ptyAttachArm(id, snap.token);
				} catch (err) {
					console.warn('[pty] attach arm failed', err);
				}
			}
		}
		return pty;
	}

	/**
	 * Subscribe to data bytes. Returns an unsubscribe function. If bytes have
	 * already been buffered (received before any subscriber attached), the new
	 * handler receives them synchronously before subscribing to the live stream.
	 *
	 * The replay is NON-destructive: the buffer stays pending until a live
	 * chunk reaches an attached subscriber (`deliverData`). A consumer that
	 * subscribes and unsubscribes without ever seeing live output — a React
	 * StrictMode double-mount, or a mount deferred behind `fonts.ready` — no
	 * longer swallows the backlog on behalf of the renderer that follows it.
	 * Every xterm that reaches `onData` in this codebase is a freshly-built,
	 * empty terminal (the cached-terminal path in `xterm-host.tsx` keeps its
	 * subscription across remounts and never re-subscribes), so re-serving the
	 * backlog to a second subscriber paints a screen that would otherwise be
	 * blank rather than double-painting a live one.
	 */
	onData(handler: PtyDataHandler): () => void {
		if (this.replayBuffer.length > 0) {
			try {
				handler(this.replayBuffer);
			} catch (err) {
				console.error('[pty] data handler threw on replay', err);
			}
		}
		this.dataSubs.add(handler);
		return () => {
			this.dataSubs.delete(handler);
		};
	}

	/**
	 * Subscribe to exit. If the PTY has already exited, the handler is fired
	 * synchronously on the next microtask with the recorded exit code.
	 */
	onExit(handler: PtyExitHandler): () => void {
		if (this.exited) {
			queueMicrotask(() => handler(this.exitCode));
			return () => {};
		}
		this.exitSubs.add(handler);
		return () => {
			this.exitSubs.delete(handler);
		};
	}

	async write(data: string): Promise<void> {
		if (this.disposed || this.exited) return;
		return ptyWrite(this.id, data);
	}

	async resize(rows: number, cols: number): Promise<void> {
		if (this.disposed || this.exited) return;
		// Active-viewer priority (D-10): Attached (non-owning) PTY viewers in an unfocused
		// window do not send ptyResize commands while another window is active, preventing
		// reflow corruption ping-pong between multi-window viewers.
		if (
			!this.owning &&
			typeof document !== 'undefined' &&
			typeof document.hasFocus === 'function' &&
			!document.hasFocus()
		) {
			return;
		}
		return ptyResize(this.id, rows, cols);
	}

	async kill(): Promise<void> {
		if (this.disposed || this.exited) return;
		return ptyKill(this.id);
	}

	/** Stop listeners and kill the PTY. Idempotent. */
	async dispose(): Promise<void> {
		if (this.disposed) return;
		this.disposed = true;
		try {
			this.unlisten?.();
		} catch {
			/* ignore */
		}
		this.unlisten = null;
		this.dataSubs.clear();
		this.exitSubs.clear();
		// Attached (non-owning) PTYs only detach their listener; the origin pane
		// owns the PTY lifecycle. Only an owning PTY kills the core process.
		if (this.owning && !this.exited) {
			await ptyKill(this.id).catch(() => {});
		}
	}
}
