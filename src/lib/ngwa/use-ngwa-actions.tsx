// Shared Ngwa actions (Scopes matrix, Installed tab, item detail).
//
// One place that turns an equipment item into the actions the locked designs
// draw — D-02's detail action row + row context menu and D-08's item header +
// ⋯ menu — each wired to the writer the Scopes matrix already uses:
//
//   - Ọba primitives: the claude-config mutation hooks (`claudePrimitive*`),
//     scope `'workspace'` = personal, `project:<id>` = a project;
//   - kernel pkgs: `pkgSetEnabled` / `pkgUninstall`;
//   - update: the Store's signed-registry path (`useStoreInstall().update`,
//     which holds back an update that asks for new permissions);
//   - Hand to Chi: the Companion dispatch-bar fill (`handToChi`).
//
// Every placing / deleting action runs the Scopes guards (`createScopeOps`)
// and sits behind the same DEC-30 confirm, so an action that is blocked in the
// matrix is blocked — with the same reason — here too. Every write
// invalidates the Ngwa snapshot afterwards, worked or not.

import { useCallback, useMemo, useState, type ReactNode } from 'react';
import { useNavigate } from '@tanstack/react-router';
import { useIsFetching, useQuery, useQueryClient } from '@tanstack/react-query';
import type { NgwaItem } from '@ikenga/contract';
import { loadHome } from '@/lib/home';
import type { NgwaStoreEntry } from '@/lib/ngwa/enrichment';
import { useStoreInstall } from '@/lib/ngwa/use-store-install';
import { ngwaSnapshotQueryKey } from '@/lib/ngwa/use-ngwa-snapshot';
import { usePaneStore } from '@/lib/panes/pane-store';
import {
	useCopyPrimitive,
	useDisablePrimitive,
	useDisablePrimitiveFor,
	useEnablePrimitive,
	useEnablePrimitiveFor,
	useMovePrimitive,
	useRemovePrimitive,
} from '@/lib/queries/claude-config';
import { useShellStore } from '@/lib/shell/shell-store';
import {
	pkgKernelStatus,
	pkgSetEnabled,
	pkgSettingsGet,
	pkgSettingsSet,
	pkgUninstall,
	type PkgSettingsField,
} from '@/lib/tauri-cmd';
import { openExternalUrl, writeClipboardText } from '@/lib/transport';
import type { PkgViewEntry } from '@/lib/pkg/use-activity-bar-entries';
import { handToChi } from '@/shell/companion/companion-store';
import {
	NgwaConfirmDialog,
	errText,
	isPkgItem,
	scopeKeyOf,
	scopeMark,
	type ConfirmRequest,
	type NgwaScopeActions,
	type ScopeColumn,
} from '@/shell/ngwa/ngwa-scope-model';
import {
	BUILTIN_REMOVE_REASON,
	BUSY_REASON,
	PKG_ONE_SCOPE,
	useScopeOps,
	type PopItem,
} from '@/shell/ngwa/ngwa-scope-ops';

// ─── Scope writers (was inline in the Scopes route) ──────────────────────────

/** Every scope writer, each followed by a snapshot invalidation. */
export function useNgwaScopeActions(): NgwaScopeActions {
	const qc = useQueryClient();
	const navigate = useNavigate();
	const enable = useEnablePrimitive();
	const disable = useDisablePrimitive();
	const copy = useCopyPrimitive();
	const move = useMovePrimitive();
	const remove = useRemovePrimitive();
	const enableFor = useEnablePrimitiveFor();
	const disableFor = useDisablePrimitiveFor();

	const refresh = useCallback(() => qc.invalidateQueries({ queryKey: ngwaSnapshotQueryKey }), [qc]);
	/** Run a mutation, then invalidate the snapshot whether it worked or not —
	 *  a partial write still changed the disk. */
	const after = useCallback(
		async <T,>(p: Promise<T>): Promise<T> => {
			try {
				return await p;
			} finally {
				void refresh();
			}
		},
		[refresh]
	);

	return useMemo<NgwaScopeActions>(
		() => ({
			enable: (kind, name, scope) => after(enable.mutateAsync({ kind, name, scope })),
			disable: (kind, name, scope) => after(disable.mutateAsync({ kind, name, scope })),
			copy: (kind, name, fromScope, toScope, opts) =>
				after(
					copy.mutateAsync({ kind, name, fromScope, toScope, overwrite: opts?.overwrite === true })
				),
			move: (kind, name, fromScope, toScope) =>
				after(move.mutateAsync({ kind, name, fromScope, toScope })),
			remove: (kind, name, scope) => after(remove.mutateAsync({ kind, name, scope })),
			enableFor: (engine, kind, name, scope) =>
				after(enableFor.mutateAsync({ engine, kind, name, scope })),
			disableFor: (engine, kind, name, scope) =>
				after(disableFor.mutateAsync({ engine, kind, name, scope })),
			pkgSetEnabled: (pkgId, enabled) => after(pkgSetEnabled(pkgId, enabled)),
			pkgUninstall: (pkgId) => after(pkgUninstall(pkgId)),
			openStore: () => void navigate({ to: '/ngwa/store', search: { kind: 'engine' } }),
		}),
		[after, enable, disable, copy, move, remove, enableFor, disableFor, navigate]
	);
}

/** Personal plus one column per live project (active first by position), and
 *  the resolved home directory — the personal scope root. Unresolved home
 *  ('' from loadHome) is `null`, and every path-checked action is then
 *  disabled with a reason instead of guessing. */
export function useNgwaScopeColumns(): { scopes: ScopeColumn[]; homeDir: string | null } {
	const projects = useShellStore((s) => s.projects);
	const activeProjectId = useShellStore((s) => s.activeProjectId);
	const home = useQuery({ queryKey: ['ngwa', 'home-dir'], queryFn: loadHome, staleTime: Infinity });

	const scopes = useMemo<ScopeColumn[]>(() => {
		const cols: ScopeColumn[] = [
			{ key: 'personal', label: 'Personal', sub: '~/.claude', active: false },
		];
		const live = projects
			.filter((p) => p.archived_at === null && p.root_path)
			.slice()
			.sort((a, b) => a.position - b.position);
		for (const p of live) {
			cols.push({
				key: `project:${p.id}`,
				label: p.display_name || p.id,
				sub: '.claude',
				active: p.id === activeProjectId,
				root: p.root_path,
			});
		}
		return cols;
	}, [projects, activeProjectId]);

	return { scopes, homeDir: home.data || null };
}

// ─── Item actions (D-02 detail row + row menu, D-08 header + ⋯ menu) ─────────

/** One action. `disabledReason` set ⇒ rendered disabled with it as the title. */
export interface NgwaAct {
	label: string;
	disabledReason?: string;
	run: () => void;
}

/** Move… / Copy to…: a scope picker. The trigger itself can be disabled. */
export interface NgwaScopePick {
	disabledReason?: string;
	title: string;
	targets: PopItem[];
}

export interface NgwaItemActionSet {
	/** Disable / Enable (D-02 key `Space`). */
	toggle: NgwaAct;
	move: NgwaScopePick;
	copy: NgwaScopePick;
	/** Row menu: move into the active project / into personal. */
	moveToProject: NgwaAct;
	moveToPersonal: NgwaAct;
	/** "Update to X" when newer; disabled "<ver> is the newest…" when current. */
	update: NgwaAct;
	/** D-02 key `↵`. */
	openFolder: NgwaAct;
	remove: NgwaAct;
	handToChi: NgwaAct;
	/** D-08: apps with `ui.views[]` only. */
	openView: NgwaAct | null;
	openManifest: NgwaAct;
	revealInstallPath: NgwaAct;
	resetSettings: NgwaAct;
	copyIyke: NgwaAct;
	/** The `iyke` line for this item. */
	iyke: string;
	/** D-08 skill variant: fill the dispatch bar with the invocation. */
	briefChi: NgwaAct | null;
}

export interface NgwaActionStatus {
	tone: 'ok' | 'err';
	text: string;
}

export interface UseNgwaItemActions {
	actionsFor: (item: NgwaItem) => NgwaItemActionSet;
	/** The confirm dialog — render once wherever the actions are used. */
	dialog: ReactNode;
	status: NgwaActionStatus | null;
}

/** The iyke command that prints one item's detail (`iyke ngwa item <id>`). */
export function iykeItemCommand(item: NgwaItem): string {
	return `iyke ngwa item ${item.id}`;
}

/** The folder an item lives in: its install path, or the folder around it
 *  when the install path is a file (agents, commands). */
export function folderOf(item: NgwaItem): string | null {
	const p = item.install_path;
	if (!p) return null;
	const base =
		p
			.replace(/[\\/]+$/, '')
			.split(/[\\/]/)
			.pop() ?? '';
	return /\.[a-z0-9]+$/i.test(base) ? p.replace(/[\\/][^\\/]*$/, '') : p;
}

function joinPath(root: string, leaf: string): string {
	return `${root.replace(/[\\/]+$/, '')}/${leaf}`;
}

/** The kernel's view registry, one query for every pkg (D-08 Open view). */
function usePkgViews() {
	return useQuery({
		queryKey: ['pkg', 'views'],
		queryFn: async () => {
			const status = await pkgKernelStatus();
			const reg = (status.registries.views ?? {}) as { entries?: PkgViewEntry[] };
			return reg.entries ?? [];
		},
		staleTime: 30_000,
	});
}

export function useNgwaItemActions({
	items,
	storeCatalog,
	unreadableSources,
	settingsItem = null,
}: {
	items: NgwaItem[];
	storeCatalog: NgwaStoreEntry[];
	unreadableSources: Array<{ source: string; error: string | null }>;
	/** The item whose settings schema Reset reads (the item detail's item). */
	settingsItem?: NgwaItem | null;
}): UseNgwaItemActions {
	const qc = useQueryClient();
	const scopeActions = useNgwaScopeActions();
	const { scopes, homeDir } = useNgwaScopeColumns();
	const activeProjectId = useShellStore((s) => s.activeProjectId);
	const store = useStoreInstall();
	const views = usePkgViews();
	const refreshing = useIsFetching({ queryKey: ngwaSnapshotQueryKey }) > 0;
	const ops = useScopeOps({ items, scopes, homeDir, unreadableSources, actions: scopeActions });

	const settingsPkg = settingsItem && isPkgItem(settingsItem) ? settingsItem.id : null;
	const settings = useQuery({
		queryKey: ['pkg', 'settings', settingsPkg],
		queryFn: () => pkgSettingsGet(settingsPkg as string),
		enabled: settingsPkg !== null,
	});

	const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
	const [status, setStatus] = useState<NgwaActionStatus | null>(null);
	const [pending, setPending] = useState(false);
	const busy = pending || refreshing;

	const exec = useCallback(async (label: string, fn: () => Promise<unknown>) => {
		setPending(true);
		setStatus(null);
		try {
			await fn();
			setStatus({ tone: 'ok', text: label });
		} catch (e) {
			setStatus({ tone: 'err', text: `${label} failed: ${errText(e)}` });
		} finally {
			setPending(false);
		}
	}, []);

	const actionsFor = useCallback(
		(item: NgwaItem): NgwaItemActionSet => {
			const pkg = isPkgItem(item);
			const key = scopeKeyOf(item.scope);
			const where = ops.scopeLabel(key);
			const label = item.display_name || item.name;
			const row = ops.rowOf(item);
			const sk = row?.storeKind ?? null;
			const fixed =
				item.kind === 'schedule'
					? `Schedules follow their pkg${item.owner_pkg_id ? ` ${item.owner_pkg_id}` : ''}; enable or disable the pkg instead`
					: item.kind === 'workflow'
						? 'Workflows have no producer yet (Phase 4)'
						: undefined;
			const noWriter = !pkg && (!row || !sk) ? 'This kind has no scope writer' : undefined;
			/** A writer is blocked while the last change settles (Scopes rule). */
			const gate = (why: string | undefined) => why ?? (busy ? BUSY_REASON : undefined);
			const open = (req: ConfirmRequest) => () => setConfirm(req);

			// ── Disable / Enable ──
			let toggle: NgwaAct;
			if (pkg) {
				const on = item.state !== 'disabled';
				toggle = {
					label: on ? 'Disable' : 'Enable',
					disabledReason: gate(undefined),
					run: () =>
						void exec(`${on ? 'Disabled' : 'Enabled'} ${label}`, () =>
							scopeActions.pkgSetEnabled(item.id, !on)
						),
				};
			} else if (fixed || noWriter || !row || !sk) {
				toggle = {
					label: item.state === 'disabled' ? 'Enable' : 'Disable',
					disabledReason: fixed ?? noWriter,
					run: () => {},
				};
			} else {
				const mark = scopeMark(row, key, ops.conflicts.get(row.key) ?? null);
				const on = mark === 'on' || mark === 'link' || mark === 'conflict';
				const scope = key === 'personal' ? 'workspace' : (key as `project:${string}`);
				toggle = on
					? {
							label: 'Disable',
							disabledReason: gate(ops.disableBlock(row, key)),
							run: () =>
								void exec(`Disabled ${label} in ${where}`, () =>
									scopeActions.disable(sk, row.name, scope)
								),
						}
					: {
							label: 'Enable',
							disabledReason: gate(ops.enableBlock(row, key)),
							run: () =>
								void exec(`Enabled ${label} in ${where}`, () =>
									scopeActions.enable(sk, row.name, scope)
								),
						};
			}

			// ── Move / Copy (primitives only: a pkg lives in one scope) ──
			const pickBlock = pkg ? PKG_ONE_SCOPE : (fixed ?? noWriter);
			function moveTo(toKey: string): Pick<PopItem, 'disabledReason' | 'onSelect'> {
				if (pickBlock || !row || !sk) return { disabledReason: pickBlock ?? noWriter };
				if (toKey === key) return { disabledReason: 'Already here' };
				const src = ops.moveSource(row, toKey, key);
				return {
					disabledReason: gate(ops.moveCopyBlock(row, toKey, key)),
					onSelect: () =>
						typeof src !== 'string' && setConfirm(ops.moveRequest(row, sk, src, toKey)),
				};
			}
			function copyTo(toKey: string): Pick<PopItem, 'disabledReason' | 'onSelect'> {
				if (pickBlock || !row || !sk) return { disabledReason: pickBlock ?? noWriter };
				if (toKey === key) return { disabledReason: 'Already here' };
				const src = ops.moveSource(row, toKey, key);
				return {
					disabledReason: gate(ops.moveCopyBlock(row, toKey, key)),
					onSelect: () =>
						typeof src !== 'string' && setConfirm(ops.copyRequest(row, sk, src, toKey)),
				};
			}
			const pick = (title: string, fn: typeof moveTo): NgwaScopePick => ({
				title,
				disabledReason: pickBlock,
				targets: scopes.map((col) => ({ label: col.label, sub: col.sub, ...fn(col.key) })),
			});
			const asAct = (lbl: string, p: Pick<PopItem, 'disabledReason' | 'onSelect'>): NgwaAct => ({
				label: lbl,
				disabledReason: p.disabledReason,
				run: () => p.onSelect?.(),
			});
			const projectKey = activeProjectId ? `project:${activeProjectId}` : null;
			const moveToProject =
				item.scope.kind !== 'personal'
					? {
							label: 'Move to project',
							disabledReason: 'Already in a project scope',
							run: () => {},
						}
					: !projectKey || !scopes.some((s) => s.key === projectKey)
						? {
								label: 'Move to project',
								disabledReason: pickBlock ?? 'No active project to move into',
								run: () => {},
							}
						: asAct('Move to project', moveTo(projectKey));
			const moveToPersonal =
				item.scope.kind === 'personal'
					? { label: 'Move to personal', disabledReason: 'Already personal', run: () => {} }
					: asAct('Move to personal', moveTo('personal'));

			// ── Update (the Store's signed-registry path) ──
			const entry = storeCatalog.find((e) => e.installedItem?.id === item.id) ?? null;
			const update: NgwaAct =
				entry?.isUpdate === true
					? {
							label: `Update to ${entry.latestVersion}`,
							disabledReason: gate(undefined),
							run: () =>
								void exec(`Updated ${label} to ${entry.latestVersion}`, () => store.update(entry)),
						}
					: {
							label: 'Update',
							disabledReason: entry
								? `${item.version ?? entry.version} is the newest published version`
								: 'Not published in the registry, so there is nothing to update to',
							run: () => {},
						};

			// ── Open folder / Hand to Chi ──
			const folder = folderOf(item);
			const openFolder: NgwaAct = folder
				? {
						label: 'Open folder',
						run: () =>
							void openExternalUrl(folder).catch((e) =>
								setStatus({ tone: 'err', text: `Open folder failed: ${errText(e)}` })
							),
					}
				: { label: 'Open folder', disabledReason: 'No install path on disk', run: () => {} };
			const chiPath = item.install_path ?? item.placements[0]?.path ?? item.id;
			const chi: NgwaAct = { label: 'Hand to Chi', run: () => handToChi(`Look at ${chiPath}`) };

			// ── Remove… (D-02 closure copy over the Scopes DEC-30 confirm) ──
			function removeConfirm(base: ConfirmRequest): ConfirmRequest {
				const deps = item.required_by.map((d) => d.name);
				return {
					...base,
					title: `Remove ${label}`,
					confirmLabel: 'Remove anyway',
					cancelLabel: 'Keep it',
					body: (
						<>
							{deps.length > 0 ? (
								<>
									<p data-remove-deps>
										<b>{deps.join(', ')}</b> requires this — remove anyway? Removing <b>{label}</b>{' '}
										leaves the closure above it incomplete until you reinstall.
									</p>
									<div className="subhead">Dependents</div>
									{deps.map((d) => (
										<div className="drow" key={d}>
											<span className="k2 mono">{d}</span>
											<span className="val warn">requires {label}</span>
										</div>
									))}
								</>
							) : (
								<p data-remove-deps="none">
									Nothing lists <b>{label}</b> in <code>requires[]</code>, so no closure breaks.
								</p>
							)}
							{base.body}
						</>
					),
				};
			}
			let remove: NgwaAct;
			if (pkg) {
				remove =
					item.origin.source === 'builtin'
						? { label: 'Remove…', disabledReason: BUILTIN_REMOVE_REASON, run: () => {} }
						: {
								label: 'Remove…',
								disabledReason: gate(undefined),
								run: open(removeConfirm(ops.pkgUninstallRequest(label, item, where))),
							};
			} else if (fixed || noWriter || !row || !sk) {
				remove = { label: 'Remove…', disabledReason: fixed ?? noWriter, run: () => {} };
			} else {
				const target = ops.claudeTarget(row, key);
				remove =
					typeof target === 'string'
						? { label: 'Remove…', disabledReason: target, run: () => {} }
						: {
								label: 'Remove…',
								disabledReason: gate(undefined),
								run: open(removeConfirm(ops.removeRequest(row, sk, key, target))),
							};
			}

			// ── D-08: Open view, ⋯ menu, Brief a Chi ──
			const view = pkg ? (views.data ?? []).find((v) => v.pkg_id === item.id) : undefined;
			const openView: NgwaAct | null = view
				? {
						label: 'Open view',
						run: () => usePaneStore.getState().navigateFocused(view.pane_route),
					}
				: null;
			const openManifest: NgwaAct = !pkg
				? {
						label: 'Open manifest.json',
						disabledReason: `A ${item.kind} has no manifest.json`,
						run: () => {},
					}
				: !item.install_path
					? {
							label: 'Open manifest.json',
							disabledReason: 'No install path on disk',
							run: () => {},
						}
					: {
							label: 'Open manifest.json',
							run: () => {
								const panes = usePaneStore.getState();
								panes.addTab(panes.focusedId, {
									kind: 'artifact',
									path: joinPath(item.install_path as string, 'manifest.json'),
								});
							},
						};
			const revealInstallPath: NgwaAct = item.install_path
				? {
						label: 'Reveal install path',
						run: () => usePaneStore.getState().revealPath(item.install_path as string),
					}
				: {
						label: 'Reveal install path',
						disabledReason: 'No install path on disk',
						run: () => {},
					};

			const resetSettings = resetAct(item, label);
			const iyke = iykeItemCommand(item);
			const copyIyke: NgwaAct = {
				label: 'Copy as iyke',
				run: () =>
					void writeClipboardText(iyke)
						.then(() => setStatus({ tone: 'ok', text: `Copied  ${iyke}` }))
						.catch((e) => setStatus({ tone: 'err', text: `Copy failed: ${errText(e)}` })),
			};
			const briefChi: NgwaAct | null =
				item.kind === 'skill' || item.kind === 'command'
					? { label: 'Brief a Chi', run: () => handToChi(`/${item.name} `) }
					: null;

			return {
				toggle,
				move: pick('Move to', moveTo),
				copy: pick('Copy to', copyTo),
				moveToProject,
				moveToPersonal,
				update,
				openFolder,
				remove,
				handToChi: chi,
				openView,
				openManifest,
				revealInstallPath,
				resetSettings,
				copyIyke,
				iyke,
				briefChi,
			};

			function resetAct(it: NgwaItem, lbl: string): NgwaAct {
				const name = 'Reset settings to defaults';
				if (!isPkgItem(it)) {
					return {
						label: name,
						disabledReason: `A ${it.kind} declares intent and has no settings`,
						run: () => {},
					};
				}
				if (settingsPkg !== it.id) {
					return {
						label: name,
						disabledReason: 'Open the item to reset its settings',
						run: () => {},
					};
				}
				if (settings.isLoading) {
					return { label: name, disabledReason: 'Reading the settings schema', run: () => {} };
				}
				if (settings.error) {
					return {
						label: name,
						disabledReason: `Settings unreadable: ${errText(settings.error)}`,
						run: () => {},
					};
				}
				const schema: PkgSettingsField[] = settings.data?.schema ?? [];
				if (schema.length === 0) {
					return {
						label: name,
						disabledReason: 'This package declares no settings in manifest.json',
						run: () => {},
					};
				}
				// Secret fields live in the vault, never in this form: untouched.
				const fields = schema.filter((f) => f.type !== 'secret');
				const resettable = fields.filter((f) => f.default !== undefined);
				const kept = fields.filter((f) => f.default === undefined).map((f) => f.label || f.key);
				if (resettable.length === 0) {
					return {
						label: name,
						disabledReason: 'No setting declares a default in manifest.json',
						run: () => {},
					};
				}
				return {
					label: name,
					disabledReason: gate(undefined),
					run: open({
						title: `Reset ${lbl}’s settings?`,
						confirmLabel: 'Reset',
						body: (
							<>
								<p>
									Every field goes back to the default in <b>manifest.json</b>. Vault keys are not
									touched — this form never held their values.
								</p>
								{kept.length > 0 && (
									<p data-reset-kept>
										{kept.join(', ')} declare{kept.length === 1 ? 's' : ''} no default and{' '}
										{kept.length === 1 ? 'is' : 'are'} left as{' '}
										{kept.length === 1 ? 'it is' : 'they are'}.
									</p>
								)}
							</>
						),
						run: async () => {
							try {
								for (const f of resettable) await pkgSettingsSet(it.id, f.key, f.default);
							} finally {
								void qc.invalidateQueries({ queryKey: ['pkg', 'settings', it.id] });
							}
						},
					}),
				};
			}
		},
		[
			ops,
			busy,
			exec,
			scopeActions,
			scopes,
			activeProjectId,
			storeCatalog,
			store,
			views.data,
			settingsPkg,
			settings.isLoading,
			settings.error,
			settings.data,
			qc,
		]
	);

	const dialog = (
		<NgwaConfirmDialog
			request={confirm}
			onClose={(result) => {
				const title = confirm?.title ?? '';
				setConfirm(null);
				if (result?.ok) setStatus({ tone: 'ok', text: `${title}: done` });
				else if (result && !result.ok)
					setStatus({ tone: 'err', text: `${title} failed: ${result.error}` });
			}}
		/>
	);

	return { actionsFor, dialog, status };
}
