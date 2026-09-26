// Ngwa Trust Review Sheet (WP-18 / locked design D-07).
//
// Displays permission consent review, update diff, or violation inspection:
// - Mode 'review' (trust-review): Standard consent review for sensitive permissions
// - Mode 'update' (trust-update): Diff highlighting newly-requested capabilities
// - Mode 'violation': Runtime security denial review with grant/block actions
//
// Adheres strictly to kernel capability boundaries (shell_execute, fs_write_outside_sandbox, net, vault_keys)
// and uses token-only styling.

import { useState } from 'react';
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
import { useMutation, useQueryClient } from '@tanstack/react-query';
import type { NgwaItem } from '@ikenga/contract';
import { pkgTrustGrant, pkgTrustRevoke } from '@/lib/tauri-cmd';
import { resolveTrustFacet } from '@/lib/ngwa/enrichment';
import './ngwa.css';

export type TrustSheetMode = 'review' | 'update' | 'violation';

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
	onApproved?: () => void;
	onDenied?: () => void;
}

export function NgwaTrustSheet({
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
