// The Store's install progress row: what replaces the Install / Update
// button while a row installs, and what a failure reads like after.
//
// Running: a step label ("Downloading · 42%", "Installing dependencies ·
// 12 packages fetched") over a determinate bar when the stage has a size, or
// an indeterminate one when it doesn't, with Cancel until registering starts.
// Failed: one readable sentence with a next step, Retry, and "Show details"
// for the deduplicated raw log and the npm debug-log path.

import { useId, useState } from 'react';
import type { InstallRun } from '@/lib/ngwa/install-progress';

function runLine(run: InstallRun): string {
	const parts = [run.cancelRequested ? 'Cancelling' : run.label];
	if (run.step && run.step.total > 1) {
		parts.push(`${run.step.pkgId} (${run.step.index + 1} of ${run.step.total})`);
	}
	if (run.percent !== null && !run.cancelRequested) parts.push(`${Math.round(run.percent)}%`);
	if (run.detail) parts.push(run.detail);
	return parts.join(' · ');
}

export function InstallProgressRow({
	run,
	onCancel,
	onRetry,
	compact = false,
}: {
	run: InstallRun;
	onCancel?: () => void;
	onRetry?: () => void;
	/** The list row's one-line form: no details panel. */
	compact?: boolean;
}) {
	const [open, setOpen] = useState(false);
	const [copied, setCopied] = useState(false);
	const detailsId = useId();

	if (run.status === 'running') {
		const line = runLine(run);
		const determinate = run.percent !== null && !run.cancelRequested;
		return (
			<div className="iprog" data-install-progress data-stage={run.stage} role="status" aria-live="polite">
				<div className="iprog-main">
					<span className="iprog-label" title={line}>
						{line}
					</span>
					<div
						className={`iprog-bar${determinate ? '' : ' indeterminate'}`}
						role="progressbar"
						aria-label={`${run.verb === 'update' ? 'Updating' : 'Installing'} ${run.name}`}
						aria-valuemin={0}
						aria-valuemax={100}
						aria-valuenow={determinate ? Math.round(run.percent ?? 0) : undefined}
						aria-valuetext={line}
					>
						<i style={determinate ? { width: `${run.percent}%` } : undefined} />
					</div>
				</div>
				{onCancel && run.cancellable && (
					<button
						type="button"
						className="btn ghost"
						data-install-cancel
						disabled={run.cancelRequested}
						onClick={onCancel}
					>
						{run.cancelRequested ? 'Cancelling…' : 'Cancel'}
					</button>
				)}
			</div>
		);
	}

	if ((run.status === 'failed' || run.status === 'cancelled') && run.error) {
		const err = run.error;
		const cancelled = run.status === 'cancelled';
		return (
			<div className="iprog failed" data-install-failed={err.kind}>
				<div className="iprog-main">
					<span className={`note ${cancelled ? '' : 'bad'}`} role={cancelled ? 'status' : 'alert'} data-action-error>
						{err.message}
					</span>
					{!compact && !cancelled && err.details && (
						<button
							type="button"
							className="iprog-toggle"
							aria-expanded={open}
							aria-controls={detailsId}
							onClick={() => setOpen((o) => !o)}
						>
							{open ? 'Hide details' : 'Show details'}
						</button>
					)}
					{open && !compact && (
						<div id={detailsId} className="iprog-details">
							{err.debugLogPath && (
								<div className="iprog-logpath">
									<span>npm log</span>
									<code title={err.debugLogPath}>{err.debugLogPath}</code>
									<button
										type="button"
										className="iprog-toggle"
										onClick={() => {
											void navigator.clipboard
												?.writeText(err.debugLogPath ?? '')
												.then(() => setCopied(true))
												.catch(() => {});
										}}
									>
										{copied ? 'Copied' : 'Copy path'}
									</button>
								</div>
							)}
							<pre>{err.details}</pre>
						</div>
					)}
				</div>
				{onRetry && err.retryable && (
					<button type="button" className="btn" data-install-retry onClick={onRetry}>
						Retry
					</button>
				)}
			</div>
		);
	}

	return null;
}
