// Settings › Integrations › Sender (box) — WP-K.
//
// Sets the box sender's credentials from the shell (plan
// 16-settings-sender-credentials.md, decisions K-1..K-11). Box only: the
// section renders nothing on the desktop or a T0 server.
//
// Values go to the SENDER's vault, never an account vault. Password / key
// inputs are write-only: type=password, autocomplete off, never prefilled
// (the server cannot read a value back), and cleared from state the moment
// Save is pressed. Status shows names, lengths and times only. Save runs the
// sender's login test first (K-3), then saves; the result is a fixed code
// shown in plain words.

import { useQueryClient } from '@tanstack/react-query';
import { Loader2, Trash2 } from 'lucide-react';
import { useState } from 'react';

import { Button } from '@/components/ui/button';
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { StatusChip } from '@/components/ui/status-chip';
import { useSenderVaultStatus } from '@/lib/queries/sender-vault';
import { queryKeys } from '@/lib/query-keys';
import {
	callSenderVault,
	isSenderVaultAdmin,
	mayManageSenderVault,
	type VaultGroupStatus,
	type VaultResult,
	type VaultStatus,
	type VaultWriteData,
} from '@/lib/transport/sender-vault';

import { SettingGroup } from './setting-group';

export interface FieldSpec {
	/** The full vault key name. */
	key: string;
	label: string;
	secret?: boolean;
	optional?: boolean;
	defaultValue?: string;
	placeholder?: string;
}

/** The login-test codes (contract 4.10), in plain words. */
export const TEST_CODE_COPY: Record<string, string> = {
	smtp_auth_failed: 'The mail server rejected the sending (SMTP) login.',
	imap_auth_failed: 'The mail server rejected the receiving (IMAP) login.',
	connect_failed:
		'Could not connect to that server. It must be a public host on a standard mail port.',
	timeout: 'The server did not answer in time.',
	tls_failed: 'The secure connection failed (certificate or TLS).',
	probe_rejected: 'The service rejected this key.',
};

const ERROR_COPY: Record<string, string> = {
	not_permitted: 'You are not allowed to change this.',
	rate_limited: 'Too many attempts. Wait a few minutes and try again.',
	vault_unconfigured: 'Setting credentials from here is not turned on for this box.',
	sender_unavailable: 'The box sender is not reachable. Nothing was changed.',
};

export function outreachFields(person: string): FieldSpec[] {
	const k = (s: string) => `OUTREACH_${person}_${s}`;
	return [
		{ key: k('SMTP_HOST'), label: 'SMTP host', placeholder: 'smtp.example.com' },
		{ key: k('SMTP_PORT'), label: 'SMTP port', defaultValue: '465' },
		{ key: k('SMTP_USER'), label: 'SMTP user', placeholder: 'you@example.com' },
		{ key: k('SMTP_PASS'), label: 'SMTP password', secret: true },
		{ key: k('IMAP_HOST'), label: 'IMAP host', placeholder: 'imap.example.com' },
		{ key: k('IMAP_PORT'), label: 'IMAP port', defaultValue: '993' },
		{ key: k('IMAP_USER'), label: 'IMAP user', placeholder: 'you@example.com' },
		{ key: k('IMAP_PASS'), label: 'IMAP password', secret: true },
	];
}

export const SHARED_CARDS: { group: string; title: string; desc: string; fields: FieldSpec[] }[] = [
	{
		group: 'ruby_mail',
		title: 'Ruby mailbox',
		desc: 'The mailbox Ruby replies from.',
		fields: [
			{ key: 'RUBY_IMAP_HOST', label: 'Mail host' },
			{ key: 'RUBY_IMAP_USER', label: 'Mail user' },
			{ key: 'RUBY_IMAP_PASS', label: 'Mail password', secret: true },
			{ key: 'RUBY_SMTP_PORT', label: 'SMTP port', optional: true, defaultValue: '465' },
			{ key: 'RUBY_IMAP_PORT', label: 'IMAP port', optional: true, defaultValue: '993' },
		],
	},
	{
		group: 'buffer',
		title: 'Buffer',
		desc: 'Social scheduling.',
		fields: [
			{ key: 'BUFFER_API_KEY', label: 'API key', secret: true },
			{ key: 'BUFFER_ORG_ID', label: 'Organization id' },
		],
	},
	{
		group: 'listmonk',
		title: 'Listmonk',
		desc: 'Newsletters. The URL must be https with no port.',
		fields: [
			{ key: 'LISTMONK_API_URL', label: 'API URL', placeholder: 'https://lists.example.com' },
			{ key: 'LISTMONK_USERNAME', label: 'Username' },
			{ key: 'LISTMONK_PASSWORD', label: 'Password', secret: true },
			{ key: 'LISTMONK_TEMPLATE_ID', label: 'Template id', optional: true },
		],
	},
	{
		group: 'cms_draft',
		title: 'Help-centre CMS draft key',
		desc: 'Format is checked only; there is no live check for this key.',
		fields: [{ key: 'CMS_MCP_AUTHORIZATION', label: 'Authorization', secret: true }],
	},
	{
		group: 'tasks',
		title: 'Tasks token',
		desc: 'The Mattermost tasks bot. The URL must be https with no port.',
		fields: [
			{ key: 'MM_URL', label: 'Mattermost URL', placeholder: 'https://chat.example.com' },
			{ key: 'TASKS_AGENT_TOKEN', label: 'Token', secret: true },
		],
	},
];

type Api = <T>(
	op: 'status' | 'set' | 'test' | 'clear',
	body?: { group?: string; values?: Record<string, string> }
) => Promise<VaultResult<T>>;

function failText(r: Extract<VaultResult<unknown>, { ok: false }>): string {
	if (r.code === 'test_failed') {
		return `${TEST_CODE_COPY[r.test ?? ''] ?? 'The login test did not pass.'} Nothing was saved. Re-enter the password to try again.`;
	}
	if (r.code === 'rate_limited' && r.retryAfterSec) {
		return `Too many attempts. Try again in about ${Math.ceil(r.retryAfterSec / 60)} min.`;
	}
	return ERROR_COPY[r.code] ?? r.message;
}

function when(iso: string | null): string {
	if (!iso) return 'by the box terminal loader, or never';
	const t = Date.parse(iso);
	return Number.isFinite(t) ? new Date(t).toLocaleString() : iso;
}

export function CredentialCard({
	title,
	desc,
	group,
	fields,
	status,
	canSet,
	api,
	onChanged,
	testId,
}: {
	title: string;
	desc: string;
	group: string;
	fields: FieldSpec[];
	status: VaultGroupStatus | undefined;
	canSet: boolean;
	api: Api;
	onChanged: () => void;
	testId: string;
}) {
	const initial = () =>
		Object.fromEntries(fields.map((f) => [f.key, f.defaultValue ?? ''])) as Record<string, string>;
	const [values, setValues] = useState<Record<string, string>>(initial);
	const [phase, setPhase] = useState<'idle' | 'saving' | 'clearing'>('idle');
	const [message, setMessage] = useState<{ tone: 'ok' | 'bad'; text: string } | null>(null);
	const [confirming, setConfirming] = useState(false);

	const present = status?.present ?? {};
	const setCount = Object.keys(present).length;
	const busy = phase !== 'idle';

	async function save() {
		const payload: Record<string, string> = {};
		for (const f of fields) {
			const v = (values[f.key] ?? '').trim();
			if (v) payload[f.key] = v;
			else if (!f.optional) {
				setMessage({ tone: 'bad', text: `Fill in every field (${f.label} is empty).` });
				return;
			}
		}
		// Write-only: the secrets leave React state before anything is sent.
		setValues((cur) =>
			Object.fromEntries(fields.map((f) => [f.key, f.secret ? '' : (cur[f.key] ?? '')]))
		);
		setMessage(null);
		// One call: the sender's `vault.set` runs the login test itself and
		// saves only if it passes, so a save is a single login attempt.
		setPhase('saving');
		const saved = await api<VaultWriteData>('set', { group, values: payload });
		setPhase('idle');
		if (!saved.ok) {
			setMessage({ tone: 'bad', text: failText(saved) });
			return;
		}
		const how = saved.data.tested;
		setMessage({
			tone: 'ok',
			text:
				how === 'shape_only'
					? 'Saved. Format checked only (no live check exists for this key).'
					: how === 'probe'
						? 'Saved. The key was checked with the service.'
						: 'Saved. The login check passed.',
		});
		onChanged();
	}

	async function clear() {
		setPhase('clearing');
		const r = await api<VaultWriteData>('clear', { group });
		setPhase('idle');
		setConfirming(false);
		if (!r.ok) {
			setMessage({ tone: 'bad', text: failText(r) });
			return;
		}
		setMessage({ tone: 'ok', text: 'Cleared.' });
		onChanged();
	}

	const readiness = status?.readiness;

	return (
		<div className="space-y-3 px-4 py-3.5 text-sm" data-testid={testId}>
			<div className="flex flex-wrap items-center justify-between gap-2">
				<div className="min-w-0">
					<div className="font-medium text-foreground">{title}</div>
					<div className="text-xs text-muted-foreground">{desc}</div>
				</div>
				<StatusChip
					tone={status?.complete ? 'live' : setCount > 0 ? 'warn' : 'muted'}
					dot
					data-testid={`${testId}-state`}
				>
					{status?.complete ? 'Set' : setCount > 0 ? 'Partly set' : 'Not set'}
				</StatusChip>
			</div>

			{status && (
				<div
					className="font-mono text-[11px] text-muted-foreground"
					data-testid={`${testId}-status`}
				>
					{fields.map((f) => (
						<div key={f.key}>
							{f.label}:{' '}
							{present[f.key] !== undefined ? `set (${present[f.key]} characters)` : 'not set'}
						</div>
					))}
					{setCount > 0 && (
						<div>
							Last changed {when(status.lastChanged)}
							{status.lastChangedBy ? ` by ${status.lastChangedBy}` : ''}
						</div>
					)}
				</div>
			)}

			{readiness && (
				<ul
					className="list-disc pl-5 text-xs text-muted-foreground"
					data-testid={`${testId}-ready`}
				>
					{readiness.identityKnown === false && (
						<li>The sender does not know this sending identity yet (a root step on the box).</li>
					)}
					{readiness.approverListed === false && (
						<li>You are not on the outreach approvers list (a root step on the box).</li>
					)}
					<li>
						Approvers, the suppression file and the sync timer are set by whoever runs the box;
						saving a mailbox only stores the login.
					</li>
				</ul>
			)}

			{canSet && (
				<form
					className="grid grid-cols-1 gap-2 sm:grid-cols-2"
					onSubmit={(e) => {
						e.preventDefault();
						void save();
					}}
				>
					{fields.map((f) => (
						<label key={f.key} className="space-y-1 text-xs text-muted-foreground">
							<span>
								{f.label}
								{f.optional ? ' (optional)' : ''}
							</span>
							<Input
								type={f.secret ? 'password' : 'text'}
								autoComplete={f.secret ? 'new-password' : 'off'}
								spellCheck={false}
								value={values[f.key] ?? ''}
								placeholder={f.secret ? 'Write-only; enter to replace' : f.placeholder}
								disabled={busy}
								onChange={(e) => setValues((cur) => ({ ...cur, [f.key]: e.target.value }))}
								className="h-8 font-mono text-xs"
								data-testid={`field-${f.key}`}
							/>
						</label>
					))}
					<p className="col-span-full text-xs text-muted-foreground">
						Saving replaces the whole group, so fill in every field. The sender tries the login
						first (no mail is sent) and saves only if it passes.
					</p>
					<div className="col-span-full flex items-center justify-end gap-2">
						<Button type="submit" size="sm" disabled={busy} data-testid={`${testId}-save`}>
							{phase === 'saving' ? <Loader2 className="mr-1.5 size-3.5 animate-spin" /> : null}
							{phase === 'saving' ? 'Checking login and saving…' : 'Test and save'}
						</Button>
					</div>
				</form>
			)}

			{message && (
				<p
					role={message.tone === 'bad' ? 'alert' : 'status'}
					className={message.tone === 'bad' ? 'text-[var(--danger)]' : 'text-muted-foreground'}
					data-testid={`${testId}-message`}
				>
					{message.text}
				</p>
			)}

			{setCount > 0 && (
				<div className="flex justify-end">
					<Button
						type="button"
						variant="outline"
						size="sm"
						disabled={busy}
						onClick={() => setConfirming(true)}
						data-testid={`${testId}-clear`}
					>
						<Trash2 className="mr-1.5 size-3.5" />
						Clear
					</Button>
				</div>
			)}

			<Dialog
				open={confirming}
				onOpenChange={(o) => !o && phase !== 'clearing' && setConfirming(false)}
			>
				<DialogContent>
					<DialogHeader>
						<DialogTitle>Clear {title}?</DialogTitle>
						<DialogDescription>
							This removes {setCount} saved {setCount === 1 ? 'key' : 'keys'} from the sender's
							vault. Anything that sends or reads through it stops until it is set again.
						</DialogDescription>
					</DialogHeader>
					<DialogFooter>
						<Button
							variant="ghost"
							disabled={phase === 'clearing'}
							onClick={() => setConfirming(false)}
						>
							Cancel
						</Button>
						<Button
							disabled={phase === 'clearing'}
							onClick={() => void clear()}
							data-testid={`${testId}-clear-confirm`}
						>
							{phase === 'clearing' ? <Loader2 className="mr-1.5 size-3.5 animate-spin" /> : null}
							Clear
						</Button>
					</DialogFooter>
				</DialogContent>
			</Dialog>
		</div>
	);
}

/** The cards for one status. Presentation only; no fetching. */
export function SenderVaultCards({
	status,
	admin,
	api,
	onChanged,
}: {
	status: VaultStatus;
	admin: boolean;
	api: Api;
	onChanged: () => void;
}) {
	const byGroup = new Map(status.groups.map((g) => [g.group, g]));
	const ownGroup = status.person ? `outreach:${status.person}` : null;
	const others = status.groups.filter(
		(g) => g.group.startsWith('outreach:') && g.group !== ownGroup
	);
	// Shared cards need both the shell's view (account admin) and the sender's.
	const showShared = admin && status.admin;

	return (
		<>
			{ownGroup && status.person ? (
				<CredentialCard
					testId="vault-outreach"
					title="My outreach mailbox"
					desc={`Your own sending mailbox (${status.person}). Each person sets their own.`}
					group={ownGroup}
					fields={outreachFields(status.person)}
					status={byGroup.get(ownGroup)}
					canSet
					api={api}
					onChanged={onChanged}
				/>
			) : (
				<div className="px-4 py-3.5 text-sm text-muted-foreground" data-testid="vault-no-slot">
					Your account has no outreach mailbox slot on the box yet. Ask whoever runs the box.
				</div>
			)}
			{showShared &&
				SHARED_CARDS.map((c) => (
					<CredentialCard
						key={c.group}
						testId={`vault-${c.group}`}
						title={c.title}
						desc={c.desc}
						group={c.group}
						fields={c.fields}
						status={byGroup.get(c.group)}
						canSet
						api={api}
						onChanged={onChanged}
					/>
				))}
			{showShared &&
				others.map((g) => (
					<CredentialCard
						key={g.group}
						testId={`vault-${g.group}`}
						title={g.label ?? g.group}
						desc="Someone else's mailbox. An admin can clear it, but only its owner can set it."
						group={g.group}
						fields={[]}
						status={g}
						canSet={false}
						api={api}
						onChanged={onChanged}
					/>
				))}
		</>
	);
}

/** The wired section. Renders nothing off the box. */
export function SenderVaultSection() {
	const queryClient = useQueryClient();
	const q = useSenderVaultStatus();
	if (!mayManageSenderVault()) return null;
	const refresh = () => void queryClient.invalidateQueries({ queryKey: queryKeys.senderVault.all });
	return (
		<div data-testid="sender-vault">
			<SettingGroup title="Sender (box)">
				<div className="px-4 py-3 text-xs leading-relaxed text-muted-foreground">
					Logins and keys for the box's mail sender. They are stored in the sender's own vault,
					never in your account, and are never shown again once saved. The terminal loaders on the
					box stay as a fallback.
				</div>
				{q.isLoading && <div className="px-4 py-3 text-sm text-muted-foreground">Loading…</div>}
				{q.data && !q.data.ok && (
					<div
						role="alert"
						className="px-4 py-3 text-sm text-muted-foreground"
						data-testid="vault-unavailable"
					>
						{ERROR_COPY[q.data.code] ?? q.data.message}
					</div>
				)}
				{q.data?.ok && (
					<SenderVaultCards
						status={q.data.status}
						admin={isSenderVaultAdmin()}
						api={callSenderVault}
						onChanged={refresh}
					/>
				)}
			</SettingGroup>
		</div>
	);
}
