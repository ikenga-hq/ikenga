// Ngwa Trust Review Sheet (WP-18 / locked design D-07).
//
// Displays permission consent review, update diff, or violation inspection:
// - Mode 'review' (trust-review): Standard consent review for sensitive permissions
// - Mode 'update' (trust-update): Diff highlighting newly-requested capabilities
// - Mode 'violation': Runtime security denial review with grant/block actions
// - Mode 'project-actions' (WP-53, G-ACTIONS §8.3): a project's untrusted
//   actions with their exact `run` text, and — DEC-65 — its held keybindings
//   with key and command; Trust pins exactly what was shown through WP-50's
//   `actions_trust_grant`. Rendered by `ProjectActionsTrustSheet` below; the
//   package modes are unchanged.
//
// Adheres strictly to kernel capability boundaries (shell_execute, fs_write_outside_sandbox, net, vault_keys)
// and uses token-only styling.

import { useEffect, useMemo, useState } from 'react';
import {
	Shield,
	Terminal,
	Folder,
	Lock,
	ExternalLink,
	AlertTriangle,
	ArrowRight,
	Check,
	X,
} from 'lucide-react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import type { NgwaItem } from '@ikenga/contract';
import { pkgTrustGrant, pkgTrustRevoke } from '@/lib/tauri-cmd';
import { resolveTrustFacet } from '@/lib/ngwa/enrichment';
import {
	actionsTrustStatus,
	readActionsFiles,
	watchActionsFiles,
	type ActionTrust,
	type ActionsTrustStatus,
	type KeybindingRule,
} from '@/lib/actions/client';
import { bindingsHash, trustShown, untrustedActions } from '@/lib/actions/runner/trust';
import './ngwa.css';

export type TrustSheetMode = 'review' | 'update' | 'violation' | 'project-actions';

/** `project-actions` mode (WP-53): which project, and which refused
 *  action(s) to put first. Matches the runner's `TrustSheetRequest`. */
export interface ProjectActionsTrustTarget {
	projectId: string | null;
	projectName?: string | null;
	actionIds?: string[];
}

export interface NgwaTrustSheetProps {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	item: NgwaItem | null;
	mode?: TrustSheetMode;
	priorVersion?: string | null;
	addedPermissions?: string[];
	violationScopeKind?: string | null;
	violationTarget?: string | null;
	/** Violation mode only: grant just the violating line instead of the
	 *  pkg-wide sensitive-perms set (WP-45 "Allow host…" →
	 *  `pkg_webview_allow_origin`). The sheet then shows only that line, hides
	 *  "Revoke trust", and its primary button runs `run`. */
	violationGrant?: { label: string; pendingLabel: string; run: () => Promise<void> };
	/** Required in `project-actions` mode (`item` is ignored there). */
	projectActions?: ProjectActionsTrustTarget;
	onApproved?: () => void;
	onDenied?: () => void;
}

export function NgwaTrustSheet(props: NgwaTrustSheetProps) {
	if (props.mode === 'project-actions') {
		if (!props.open || !props.projectActions) return null;
		return (
			<ProjectActionsTrustSheet
				target={props.projectActions}
				onOpenChange={props.onOpenChange}
				onApproved={props.onApproved}
			/>
		);
	}
	return <PkgTrustSheet {...props} />;
}

function PkgTrustSheet({
	open,
	onOpenChange,
	item,
	mode = 'review',
	priorVersion,
	addedPermissions = [],
	violationScopeKind,
	violationTarget,
	violationGrant,
	onApproved,
	onDenied,
}: NgwaTrustSheetProps) {
	const scopedGrant = mode === 'violation' ? violationGrant : undefined;
	const qc = useQueryClient();
	const [actionError, setActionError] = useState<string | null>(null);

	const grantMutation = useMutation({
		mutationFn: async () => {
			if (!item) return;
			if (scopedGrant) {
				await scopedGrant.run();
				return;
			}
			await pkgTrustGrant(item.id, item.version ?? '0.0.0');
		},
		onSuccess: () => {
			void qc.invalidateQueries({ queryKey: ['ngwa'] });
			onApproved?.();
			onOpenChange(false);
		},
		onError: (err: Error) => {
			setActionError(err.message);
		},
	});

	const revokeMutation = useMutation({
		mutationFn: async () => {
			if (!item) return;
			await pkgTrustRevoke(item.id);
		},
		onSuccess: () => {
			void qc.invalidateQueries({ queryKey: ['ngwa'] });
			onDenied?.();
			onOpenChange(false);
		},
		onError: (err: Error) => {
			setActionError(err.message);
		},
	});

	if (!open || !item) return null;

	const perms = item.trust.perms;
	const shellExec = perms?.shell_execute ?? [];
	const fsWrite = perms?.fs_write_outside_sandbox ?? [];
	const net = perms?.net ?? [];
	const vault = perms?.vault_keys ?? [];
	const totalSensitive = shellExec.length + fsWrite.length + net.length + vault.length;

	const trustFacet = resolveTrustFacet(item.trust);

	return (
		<div className="trust-overlay" role="dialog" aria-modal="true" aria-labelledby="trust-sheet-title">
			<div className="trust-sheet">
				{/* ── Sheet Header ── */}
				<header className="trust-head">
					<div className="flex items-center gap-2">
						<div className="mark">
							<Shield className="h-5 w-5 text-primary" />
						</div>
						<div>
							<h2 id="trust-sheet-title" className="text-base font-semibold">
								{mode === 'review' && `Review permissions · ${item.display_name || item.name}`}
								{mode === 'update' && `Permission change · ${item.display_name || item.name}`}
								{mode === 'violation' && `Security violation · ${item.display_name || item.name}`}
							</h2>
							<div className="text-xs text-muted-foreground flex items-center gap-2 mt-0.5">
								<span className="mono">{item.id}</span>
								<span>·</span>
								<span>status: <b className={`badge t-${trustFacet}`}>{trustFacet}</b></span>
								{item.version && <span>· v{item.version}</span>}
							</div>
						</div>
					</div>
					<button
						type="button"
						className="btn ghost icon-only"
						onClick={() => onOpenChange(false)}
						aria-label="Close sheet"
					>
						<X className="h-4 w-4" />
					</button>
				</header>

				{/* ── Body ── */}
				<div className="trust-body sc">
					{actionError && (
						<div className="p-3 mb-3 bg-destructive/10 border border-destructive/30 rounded text-destructive text-xs">
							{actionError}
						</div>
					)}

					{/* ── Mode 1: Update Diff ── */}
					{mode === 'update' && (
						<div className="mb-4">
							<div className="p-3 bg-warning-soft border border-warning-soft rounded flex items-start gap-2 mb-3">
								<AlertTriangle className="h-4 w-4 text-warning flex-none mt-0.5" />
								<div className="text-xs">
									<strong>Version upgrade ({priorVersion ?? 'prior'} <ArrowRight className="inline h-3 w-3" /> v{item.version})</strong>
									<p className="mt-1">
										This version update requests additional capabilities outside your previous grant.
									</p>
								</div>
							</div>

							{addedPermissions.length > 0 && (
								<div className="mb-3">
									<div className="text-xs font-semibold uppercase tracking-wider text-warning mb-1.5">
										Newly requested permissions ({addedPermissions.length})
									</div>
									<div className="space-y-1.5">
										{addedPermissions.map((perm) => (
											<div key={perm} className="prow2 sensitive border-warning/40">
												<AlertTriangle className="h-4 w-4 text-warning flex-none" />
												<span className="pt">
													<span className="p1">{perm}</span>
												</span>
											</div>
										))}
									</div>
								</div>
							)}
						</div>
					)}

					{/* ── Mode 2: Violation ── */}
					{mode === 'violation' && (
						<div className="mb-4">
							<div className="p-3 bg-destructive/10 border border-destructive/30 rounded flex items-start gap-2 mb-3">
								<AlertTriangle className="h-4 w-4 text-destructive flex-none mt-0.5" />
								<div className="text-xs">
									<strong>Blocked ungranted action</strong>
									<p className="mt-1">
										The package attempted an operation outside its authorized permissions:
									</p>
									<div className="mt-2 font-mono bg-background/80 p-2 rounded border border-border">
										<div>scope: <b>{violationScopeKind || 'unknown'}</b></div>
										{violationTarget && <div>target: <b>{violationTarget}</b></div>}
									</div>
								</div>
							</div>
						</div>
					)}

					{/* ── Standard Sensitive Permissions List ── */}
					{!scopedGrant && (
					<div className="mb-4">
						<div className="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
							Declared sensitive capabilities ({totalSensitive})
						</div>

						{totalSensitive === 0 ? (
							<div className="p-3 text-xs text-muted-foreground bg-muted/20 rounded border border-border">
								No sensitive permissions requested. This package runs within standard process boundaries.
							</div>
						) : (
							<div className="space-y-2">
								{shellExec.map((cmd) => (
									<div key={cmd} className="prow2 sensitive">
										<Terminal className="h-4 w-4 flex-none" />
										<span className="pt">
											<span className="p1">shell:exec · {cmd}</span>
											<span className="p2">Execute command in local subshell</span>
										</span>
									</div>
								))}
								{fsWrite.map((path) => (
									<div key={path} className="prow2 sensitive">
										<Folder className="h-4 w-4 flex-none" />
										<span className="pt">
											<span className="p1">fs:write · {path}</span>
											<span className="p2">Write to disk outside isolated package folder</span>
										</span>
									</div>
								))}
								{net.map((domain) => (
									<div key={domain} className="prow2">
										<ExternalLink className="h-4 w-4 flex-none" />
										<span className="pt">
											<span className="p1">net · {domain}</span>
											<span className="p2">Connect to external internet domain</span>
										</span>
									</div>
								))}
								{vault.map((key) => (
									<div key={key} className="prow2 sensitive">
										<Lock className="h-4 w-4 flex-none" />
										<span className="pt">
											<span className="p1">vault · {key}</span>
											<span className="p2">Access encrypted vault credentials</span>
										</span>
									</div>
								))}
							</div>
						)}
					</div>
					)}

					<div className="text-xs text-muted-foreground bg-muted/20 p-2.5 rounded border border-border">
						<Shield className="h-3.5 w-3.5 inline mr-1 text-primary" />
						{scopedGrant
							? 'Approval grants only the target above, on top of what the package declares. Nothing else changes.'
							: 'Approval grants these capabilities to the package runtime. Consent can be revoked at any time.'}
					</div>
				</div>

				{/* ── Sheet Footer ── */}
				<footer className="trust-foot">
					<button
						type="button"
						className="btn ghost"
						onClick={() => onOpenChange(false)}
					>
						Cancel
					</button>

					{!scopedGrant &&
					(item.trust.state === 'granted' || item.trust.state === 'auto_granted') ? (
						<button
							type="button"
							className="btn danger"
							disabled={revokeMutation.isPending}
							onClick={() => revokeMutation.mutate()}
						>
							{revokeMutation.isPending ? 'Revoking…' : 'Revoke trust'}
						</button>
					) : null}

					<button
						type="button"
						className="btn primary"
						disabled={grantMutation.isPending}
						onClick={() => grantMutation.mutate()}
					>
						<Check className="h-3.5 w-3.5 mr-1" />
						{scopedGrant
							? grantMutation.isPending
								? scopedGrant.pendingLabel
								: scopedGrant.label
							: grantMutation.isPending
							? 'Approving…'
							: mode === 'update'
								? 'Approve update'
								: 'Grant permissions'}
					</button>
				</footer>
			</div>
		</div>
	);
}

// ─── project-actions mode (WP-53 · DEC-55 · DEC-65) ─────────────────────────

/** The one-line "what runs" headline; the full `run` JSON is shown below it. */
export function runHeadline(entry: Pick<ActionTrust, 'kind' | 'run'>): string {
	const run = entry.run;
	switch (run.kind) {
		case 'shell':
			return run.command;
		case 'iyke':
			return `${run.method ?? 'POST'} ${run.route}`;
		case 'skill':
			return `skill ${run.skill}`;
		case 'workflow':
			return `workflow ${run.workflow}`;
		case 'chi':
			return run.prompt;
		case 'open':
			return run.url;
	}
}

export function bindingLine(rule: KeybindingRule): string {
	const parts = [rule.key, '→', rule.command.startsWith('-') ? `unbind ${rule.command.slice(1)}` : rule.command];
	if (rule.when) parts.push(`when ${rule.when}`);
	if (rule.platform) parts.push(`(${rule.platform} only)`);
	return parts.join(' ');
}

interface ProjectTrustData {
	status: ActionsTrustStatus;
	/** The project's rules, as held (DEC-65). */
	bindings: KeybindingRule[];
	/** The rules shown hash to the pin `status` offers (else they moved). */
	bindingsMatch: boolean;
}

async function loadProjectTrust(projectId: string | null): Promise<ProjectTrustData> {
	// Status first, then the file: if the file moves in between, the shown
	// rules no longer hash to `status.keybindings.hash` and trusting them is
	// disabled (and the grant itself refuses a stale hash).
	const status = await actionsTrustStatus(projectId);
	const files = await readActionsFiles(projectId);
	const bindings = files.project?.keybindings.document?.bindings ?? [];
	const shown = await bindingsHash(bindings);
	return { status, bindings, bindingsMatch: shown === status.keybindings.hash };
}

function ProjectActionsTrustSheet({
	target,
	onOpenChange,
	onApproved,
}: {
	target: ProjectActionsTrustTarget;
	onOpenChange: (open: boolean) => void;
	onApproved?: () => void;
}) {
	const qc = useQueryClient();
	const queryKey = ['actions', 'trust-sheet', target.projectId ?? null] as const;
	const query = useQuery({ queryKey, queryFn: () => loadProjectTrust(target.projectId ?? null) });
	const [excluded, setExcluded] = useState<Set<string>>(() => new Set());
	const [skipKeybindings, setSkipKeybindings] = useState(false);
	const [actionError, setActionError] = useState<string | null>(null);

	// A file edit or a trust change elsewhere re-reads what is shown.
	useEffect(() => {
		let stop: (() => void) | null = null;
		let live = true;
		void watchActionsFiles(() => {
			void qc.invalidateQueries({ queryKey: ['actions', 'trust-sheet', target.projectId ?? null] });
		}).then((unlisten) => {
			if (live) stop = unlisten;
			else unlisten();
		});
		return () => {
			live = false;
			stop?.();
		};
	}, [qc, target.projectId]);

	const data = query.data;
	const pending = useMemo(() => {
		if (!data) return [];
		const focus = new Set(target.actionIds ?? []);
		const list = untrustedActions(data.status);
		return [...list.filter((e) => focus.has(e.id)), ...list.filter((e) => !focus.has(e.id))];
	}, [data, target.actionIds]);
	const kb = data?.status.keybindings;
	const keybindingsHeld = kb?.state === 'untrusted' || kb?.state === 'changed';

	const grant = useMutation({
		mutationFn: async () => {
			if (!data) return;
			const actions = pending.filter((entry) => !excluded.has(entry.id));
			const keybindingsHash =
				keybindingsHeld && !skipKeybindings && data.bindingsMatch ? (kb?.hash ?? null) : null;
			await trustShown(target.projectId ?? null, { actions, keybindingsHash });
		},
		onSuccess: () => {
			void qc.invalidateQueries({ queryKey: ['actions'] });
			onApproved?.();
			onOpenChange(false);
		},
		onError: (err: Error) => {
			setActionError(err.message);
			void qc.invalidateQueries({ queryKey });
		},
	});

	const chosenActions = pending.filter((entry) => !excluded.has(entry.id)).length;
	const chosenKeybindings = keybindingsHeld && !skipKeybindings && Boolean(data?.bindingsMatch);
	const nothingChosen = chosenActions === 0 && !chosenKeybindings;
	const title = target.projectName ? `Trust project actions · ${target.projectName}` : 'Trust project actions';

	return (
		<div className="trust-overlay" role="dialog" aria-modal="true" aria-labelledby="trust-sheet-title">
			<div className="trust-sheet" data-mode="project-actions">
				<header className="trust-head">
					<div className="flex items-center gap-2">
						<div className="mark">
							<Shield className="h-5 w-5 text-primary" />
						</div>
						<div>
							<h2 id="trust-sheet-title" className="text-base font-semibold">
								{title}
							</h2>
							<div className="text-xs text-muted-foreground mt-0.5">
								{data ? <span className="mono">{data.status.projectRoot}</span> : 'Loading…'}
							</div>
						</div>
					</div>
					<button
						type="button"
						className="btn ghost icon-only"
						onClick={() => onOpenChange(false)}
						aria-label="Close sheet"
					>
						<X className="h-4 w-4" />
					</button>
				</header>

				<div className="trust-body sc">
					{(actionError || query.error) && (
						<div className="p-3 mb-3 bg-destructive/10 border border-destructive/30 rounded text-destructive text-xs">
							{actionError ?? (query.error instanceof Error ? query.error.message : String(query.error))}
						</div>
					)}

					{data && (data.status.actionsError || data.status.keybindingsError) && (
						<div className="p-3 mb-3 bg-warning-soft border border-warning-soft rounded flex items-start gap-2 text-xs">
							<AlertTriangle className="h-4 w-4 text-warning flex-none mt-0.5" />
							<div>
								{data.status.actionsError && <p>actions.json: {data.status.actionsError}</p>}
								{data.status.keybindingsError && <p>keybindings.json: {data.status.keybindingsError}</p>}
								{(data.status.actionsStale || data.status.keybindingsStale) && (
									<p className="mt-1">Showing the last valid version, which is the one in force.</p>
								)}
							</div>
						</div>
					)}

					{data && (
						<div className="mb-4">
							<div className="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-2">
								Actions waiting for trust ({pending.length})
							</div>
							{pending.length === 0 ? (
								<div className="p-3 text-xs text-muted-foreground bg-muted/20 rounded border border-border">
									Every shell, iyke, skill and workflow action in this project is trusted.
								</div>
							) : (
								<div className="space-y-2">
									{pending.map((entry) => (
										<div
											key={entry.id}
											className="prow2 sensitive items-start"
											data-action-id={entry.id}
										>
											<input
												type="checkbox"
												className="mt-1"
												checked={!excluded.has(entry.id)}
												onChange={(e) => {
													const next = new Set(excluded);
													if (e.target.checked) next.delete(entry.id);
													else next.add(entry.id);
													setExcluded(next);
												}}
												aria-label={`Trust ${entry.name ?? entry.id}`}
											/>
											<Terminal className="h-4 w-4 flex-none mt-0.5" />
											<span className="pt min-w-0">
												<span className="p1">
													{entry.name ?? entry.id} · <span className="mono">{entry.id}</span> · {entry.kind}{' '}
													<b className={`badge t-${entry.state === 'changed' ? 'changed' : 'untrusted'}`}>
														{entry.state === 'changed' ? 'changed since trusted' : 'new'}
													</b>
												</span>
												<span className="p2 mono break-all">{runHeadline(entry)}</span>
												<pre className="mono text-[11px] mt-1 p-2 rounded border border-border bg-background/80 whitespace-pre-wrap break-all">
													{JSON.stringify(entry.run, null, 2)}
												</pre>
											</span>
										</div>
									))}
								</div>
							)}
						</div>
					)}

					{data && keybindingsHeld && (
						<div className="mb-4">
							<div className="text-xs font-semibold uppercase tracking-wider text-warning mb-2">
								Keybindings held until trusted ({data.bindings.length})
							</div>
							<div className="prow2 sensitive items-start" data-held-keybindings="">
								<input
									type="checkbox"
									className="mt-1"
									checked={!skipKeybindings && data.bindingsMatch}
									disabled={!data.bindingsMatch}
									onChange={(e) => setSkipKeybindings(!e.target.checked)}
									aria-label="Trust these keybindings"
								/>
								<Lock className="h-4 w-4 flex-none mt-0.5" />
								<span className="pt min-w-0">
									<span className="p1">
										.ikenga/keybindings.json{' '}
										<b className={`badge t-${kb?.state === 'changed' ? 'changed' : 'untrusted'}`}>
											{kb?.state === 'changed' ? 'changed since trusted' : 'new'}
										</b>
									</span>
									<span className="p2">
										{data.bindingsMatch
											? 'Until trusted these rules fire nothing and unbind nothing.'
											: 'The file changed while this sheet loaded — reopen it to review the current rules.'}
									</span>
									<ul className="mono text-[11px] mt-1 space-y-0.5">
										{data.bindings.map((rule, index) => (
											<li key={`${index}:${rule.key}:${rule.command}`} className="break-all">
												{bindingLine(rule)}
											</li>
										))}
									</ul>
								</span>
							</div>
						</div>
					)}

					<div className="text-xs text-muted-foreground bg-muted/20 p-2.5 rounded border border-border">
						<Shield className="h-3.5 w-3.5 inline mr-1 text-primary" />
						Trust is pinned to exactly the text above and kept outside the project folder. Any later edit
						asks again. Trusting keybindings does not trust actions, and the reverse.
					</div>
				</div>

				<footer className="trust-foot">
					<button type="button" className="btn ghost" onClick={() => onOpenChange(false)}>
						Cancel
					</button>
					<button
						type="button"
						className="btn primary"
						disabled={!data || nothingChosen || grant.isPending}
						onClick={() => {
							setActionError(null);
							grant.mutate();
						}}
					>
						<Check className="h-3.5 w-3.5 mr-1" />
						{grant.isPending ? 'Trusting…' : 'Trust'}
					</button>
				</footer>
			</div>
		</div>
	);
}
