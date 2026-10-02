// Policies tab — D-05 `policies` (`designs/people.html`), WP-76
// (G-ACCESS §4.1, §5.2, §15 N-3).
//
// - "What each role may do": the matrix `access_policy_get` returns — §4.1's
//   defaults with this project's overrides. Owner is fixed; `secrets` is
//   never grantable to a non-Owner (a shield, not a cell). The Owner, with a
//   password session or a Full device, clicks a cell to allow or withhold it
//   (`access_policy_set_cell`).
// - Limits: "Require Owner approval" (`access_policy_set_owner_approval`,
//   default on). Off adds "Asks that touch secrets still go to you." (D-15,
//   N-10). The engine spend cap renders disabled, "not enforced yet" (D-11,
//   N-3).
// - T0 (§4.5.5): the default matrix read-only, with "Roles apply to people
//   you share a project with on an Ikenga server."; the approval toggle is
//   hidden (only the Owner exists).

import { Check, Minus, ShieldAlert } from 'lucide-react';
import { useCallback, useEffect, useState } from 'react';

import { Switch } from '@/components/ui/switch';
import { cn } from '@/components/ui/utils';
import {
	CAP_LABELS,
	CAPS,
	type Cap,
	ROLE_DEFAULT_CAPS,
	ROLES,
	type Role,
} from '@/lib/access/caps.gen';
import {
	accessPolicyGet,
	accessPolicySetCell,
	accessPolicySetOwnerApproval,
	parseAccessError,
} from '@/lib/access/client';
import { currentShare } from '@/lib/transport';

import { Kv, PeopleBlock, PeopleHeader, PeopleRow } from './frame';
import {
	type MembersList,
	personName,
	ROLE_TITLES,
	useAccessStatus,
	useMembersList,
	useTabProject,
} from './members';

export type Cell = 'allowed' | 'withheld' | 'never';
export type Matrix = Record<Role, Record<Cap, Cell>>;

/** §4.1's defaults as a matrix (T0, and before the store answers). */
export function defaultMatrix(): Matrix {
	const out = {} as Matrix;
	for (const role of ROLES) {
		const row = {} as Record<Cap, Cell>;
		for (const cap of CAPS) {
			row[cap] =
				role !== 'owner' && cap === 'secrets'
					? 'never'
					: ROLE_DEFAULT_CAPS[role].includes(cap)
						? 'allowed'
						: 'withheld';
		}
		out[role] = row;
	}
	return out;
}

/** Whether a cell can be clicked: never the Owner's, never `secrets`. */
export function cellEditable(role: Role, cap: Cap): boolean {
	return role !== 'owner' && cap !== 'secrets';
}

/** "1 person" / "2 people" under a role's column. */
export function roleCount(list: MembersList | null, role: Role): string | null {
	if (role === 'owner') return 'you';
	if (!list) return null;
	const n = list.members.filter((m) => m.role === role).length;
	return `${n} ${n === 1 ? 'person' : 'people'}`;
}

/** D-15: the extra line when "Require Owner approval" is off. */
export const SECRETS_STILL_GO_TO_YOU = 'Asks that touch secrets still go to you.';

export function PoliciesTab() {
	const { status, tier, loaded } = useAccessStatus();
	const { projectId, projectName } = useTabProject();
	const share = currentShare();
	const [matrix, setMatrix] = useState<Matrix>(defaultMatrix);
	const [ownerApproval, setOwnerApproval] = useState(true);
	const [error, setError] = useState<string | null>(null);
	const { list } = useMembersList(projectId, loaded && tier === 't1' && !share);
	const isOwner = tier === 't1' && !share;
	const canEdit = isOwner && status?.adminStrength === true;

	const load = useCallback(async () => {
		try {
			const p = await accessPolicyGet(projectId);
			// A host without the access store answers nothing: the defaults.
			setMatrix(p?.matrix ? (p.matrix as Matrix) : defaultMatrix());
			setOwnerApproval(p?.ownerApprovalRequired ?? true);
			setError(null);
		} catch (e) {
			const { code, message } = parseAccessError(e);
			setMatrix(defaultMatrix());
			if (code !== 'store_unavailable' && code !== 'requires_t1') setError(message);
		}
	}, [projectId]);
	useEffect(() => {
		if (loaded) void load();
	}, [loaded, load]);

	const toggle = async (role: Role, cap: Cap) => {
		if (!canEdit || !cellEditable(role, cap)) return;
		const allowed = matrix[role][cap] !== 'allowed';
		setError(null);
		try {
			const m = await accessPolicySetCell(
				projectId,
				role as Exclude<Role, 'owner'>,
				cap as Exclude<Cap, 'secrets'>,
				allowed
			);
			setMatrix(m as Matrix);
		} catch (e) {
			setError(parseAccessError(e).message);
		}
	};
	const setApproval = async (required: boolean) => {
		setError(null);
		const before = ownerApproval;
		setOwnerApproval(required);
		try {
			await accessPolicySetOwnerApproval(projectId, required);
		} catch (e) {
			setOwnerApproval(before);
			setError(parseAccessError(e).message);
		}
	};

	return (
		<div data-state="policies" className="mx-auto w-full max-w-[960px] space-y-4 px-6 py-6">
			<PeopleHeader tab="policies" />
			<PeopleBlock
				title="What each role may do"
				right={<Kv>{share?.projectName ?? projectName}</Kv>}
			>
				<div className="-mx-3 overflow-x-auto">
					<table
						className="w-full border-collapse text-left text-[var(--text-caption,12px)]"
						data-matrix
					>
						<thead>
							<tr>
								<th className="px-3 py-1.5 font-semibold text-[var(--fg)]">Capability</th>
								{ROLES.map((r) => (
									<th
										key={r}
										className="w-[104px] px-3 py-1.5 text-center font-semibold text-[var(--fg)]"
									>
										{ROLE_TITLES[r]}
										{tier === 't1' && roleCount(list, r) && (
											<span className="block font-mono text-[var(--text-micro)] font-normal text-[var(--fg-muted)]">
												{roleCount(list, r)}
											</span>
										)}
									</th>
								))}
							</tr>
						</thead>
						<tbody>
							{CAPS.map((cap) => (
								<tr key={cap} className="border-t border-[var(--border-soft)]">
									<td className="px-3 py-1.5">
										<span className="block text-[var(--fg)]">{CAP_LABELS[cap].label}</span>
										<span className="block text-[var(--text-micro)] text-[var(--fg-muted)]">
											{CAP_LABELS[cap].sub}
										</span>
									</td>
									{ROLES.map((role) => (
										<td
											key={role}
											className="border-l border-[var(--border-soft)] px-3 py-1.5 text-center"
										>
											<MatrixCell
												role={role}
												cap={cap}
												value={matrix[role][cap]}
												editable={canEdit && cellEditable(role, cap)}
												onToggle={() => void toggle(role, cap)}
											/>
										</td>
									))}
								</tr>
							))}
						</tbody>
					</table>
				</div>
				<p className="m-0 max-w-[560px] py-3 text-[var(--text-caption,12px)] leading-relaxed text-[var(--fg-muted)]">
					{tier === 't0' ? (
						'Roles apply to people you share a project with on an Ikenga server.'
					) : canEdit ? (
						<>
							Click a cell to allow or withhold it. Owner is fixed.{' '}
							<b className="font-semibold text-[var(--fg)]">Read secrets</b> is never grantable: the
							vault stays on the host.
						</>
					) : (
						<>
							Owner is fixed. <b className="font-semibold text-[var(--fg)]">Read secrets</b> is
							never grantable: the vault stays on the host.
						</>
					)}
				</p>
				{error && (
					<p role="alert" className="m-0 pb-3 text-[12px] text-[var(--danger)]">
						{error}
					</p>
				)}
			</PeopleBlock>
			{tier === 't1' && (
				<PeopleBlock title="Limits">
					<PeopleRow
						label="Require Owner approval"
						top
						sub="Sensitive permission asks — secrets, shell exec, writes outside the project — come to you even if an Operator is watching."
					>
						<Switch
							checked={ownerApproval}
							disabled={!canEdit}
							aria-label="Require Owner approval"
							onCheckedChange={(v) => void setApproval(v)}
						/>
						<Kv>{ownerApproval ? 'on' : 'off'}</Kv>
						{!ownerApproval && (
							<span
								className="basis-full text-[var(--text-micro)] text-[var(--fg-muted)]"
								data-secrets-line
							>
								Operators may answer sensitive asks. {SECRETS_STILL_GO_TO_YOU}
							</span>
						)}
					</PeopleRow>
					<PeopleRow
						label="Engine spend cap"
						top
						sub="Per member, per week. Stored, not enforced yet — engines run and bill as the Owner."
					>
						<div className="flex w-full flex-col gap-1.5" data-spend-cap>
							{(list?.members ?? []).length === 0 ? (
								<Kv>Nobody to cap yet.</Kv>
							) : (
								(list?.members ?? []).map((m) => (
									<div key={m.principalId} className="flex items-center gap-3">
										<span className="w-[180px] truncate font-mono text-[var(--text-micro)] text-[var(--fg)]">
											{personName(m)}
										</span>
										<span className="w-[72px] text-[var(--text-micro)] text-[var(--fg-muted)]">
											{ROLE_TITLES[m.role]}
										</span>
										<input
											disabled
											aria-label={`Weekly spend cap for ${personName(m)}`}
											value={
												m.weeklySpendCapCents === null ? '' : String(m.weeklySpendCapCents / 100)
											}
											placeholder="$ —"
											className="h-7 w-[90px] rounded-[var(--radius-sm)] border border-[var(--border)] bg-[var(--bg-sunken)] px-2 font-mono text-[12px] text-[var(--fg-muted)] opacity-60"
										/>
										<Kv>not enforced yet</Kv>
									</div>
								))
							)}
						</div>
					</PeopleRow>
				</PeopleBlock>
			)}
		</div>
	);
}

function MatrixCell({
	role,
	cap,
	value,
	editable,
	onToggle,
}: {
	role: Role;
	cap: Cap;
	value: Cell;
	editable: boolean;
	onToggle: () => void;
}) {
	const label = `${ROLE_TITLES[role]} · ${CAP_LABELS[cap].label}: ${value}`;
	const icon =
		value === 'never' ? (
			<ShieldAlert className="h-3.5 w-3.5 text-[var(--danger)]" />
		) : value === 'allowed' ? (
			<Check className="h-3.5 w-3.5 text-[var(--live)]" />
		) : (
			<Minus className="h-3.5 w-3.5 text-[var(--fg-muted)]" />
		);
	if (!editable) {
		return (
			<span
				className="inline-flex items-center justify-center"
				role="img"
				aria-label={label}
				title={value === 'never' ? 'Never grantable — the vault stays on the host' : undefined}
				data-cell={value}
			>
				{icon}
			</span>
		);
	}
	return (
		<button
			type="button"
			aria-label={label}
			aria-pressed={value === 'allowed'}
			onClick={onToggle}
			data-cell={value}
			className={cn(
				'inline-flex h-6 w-6 items-center justify-center rounded-[var(--radius-sm)] hover:bg-[var(--bg-sunken)] focus-visible:ring-2 focus-visible:ring-[var(--primary-soft)]'
			)}
		>
			{icon}
		</button>
	);
}
