// WP-17: "Get a link" — an outside link to one file (box-files, 06-file-links-security).
// Expiry (1 / 7 / 30 days / never, default 7), an optional passcode, and a
// one-time box: the link is shown ONCE with Copy and a plain note that it
// cannot be shown again (only its hash is stored). Rendered inside the share
// sheet; never shown unless the server turned file links on.

import { Copy, ShieldAlert } from 'lucide-react';
import { useId, useState } from 'react';

import { Button } from '@/components/ui/button';
import { cn } from '@/components/ui/utils';
import { accessFileLinkCreate, type FileLinkCreated, parseAccessError } from '@/lib/access/client';
import { copyText } from '@/lib/clipboard';

import {
	createArgs,
	DEFAULT_LINK_DAYS,
	LINK_EXPIRY_CHOICES,
	linkBlocker,
	passcodeProblem,
} from './file-link-model';

const FIELD =
	'h-8 rounded-[var(--radius-sm)] border border-[var(--border)] bg-[var(--bg-sunken)] px-2 text-[length:var(--text-caption,12px)] text-[var(--fg)] outline-none focus:border-[var(--primary)]';

export const LINK_ONCE_NOTE =
	"This link can't be shown again. Copy it now. If you lose it, make a new link and revoke this one.";

export function OutsideLinkForm({
	path,
	onCreated,
}: {
	/** Absolute path of the file on the server. */
	path: string;
	/** Called after a link is made (e.g. to refresh the Links panel). */
	onCreated?: (link: FileLinkCreated) => void;
}) {
	const [days, setDays] = useState<number | null>(DEFAULT_LINK_DAYS);
	const [passcode, setPasscode] = useState('');
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const [created, setCreated] = useState<FileLinkCreated | null>(null);
	const [copied, setCopied] = useState(false);
	const ids = { expires: useId(), passcode: useId() };

	const blocker = linkBlocker({ path, days, passcode });
	const shownProblem = passcodeProblem(passcode);

	const create = async () => {
		setBusy(true);
		setError(null);
		try {
			const out = await accessFileLinkCreate(createArgs({ path, days, passcode }));
			// A relative URL (no public address configured server-side) still copies as a full one.
			setCreated({ ...out, url: new URL(out.url, window.location.origin).toString() });
			setPasscode('');
			onCreated?.(out);
		} catch (e) {
			setError(parseAccessError(e).message);
		} finally {
			setBusy(false);
		}
	};

	const copy = async () => {
		if (!created) return;
		setCopied(await copyText(created.url));
	};

	if (created) {
		return (
			<div
				data-state="file-link-created"
				className="space-y-2 rounded-[var(--radius-md,6px)] border border-[var(--border)] bg-[var(--bg-sunken)] p-3"
			>
				<div className="flex items-center gap-2">
					<code className="min-w-0 flex-1 truncate font-mono text-[11px] text-[var(--fg)]">
						{created.url}
					</code>
					<Button type="button" size="xs" variant="outline" onClick={() => void copy()}>
						<Copy /> {copied ? 'Copied' : 'Copy link'}
					</Button>
				</div>
				<p className="m-0 flex items-start gap-1.5 text-[length:var(--text-micro)] text-[var(--fg)]" data-link-once>
					<ShieldAlert className="mt-0.5 h-3.5 w-3.5 flex-none text-[var(--danger)]" />
					{LINK_ONCE_NOTE}
				</p>
				<p className="m-0 text-[length:var(--text-micro)] text-[var(--fg-muted)]">
					{created.name} · anyone with the link can open it
					{created.hasPasscode ? ', after the passcode' : ''}. Revoke it any time under Settings, People,
					Profile.
				</p>
			</div>
		);
	}

	return (
		<div data-state="file-link-form" className="space-y-3">
			<p className="m-0 text-[length:var(--text-caption,12px)] text-[var(--fg-muted)]">
				Anyone with the link can open this one file, with no account. Every open is recorded and you are
				told the first time it is opened.
			</p>
			<div className="flex min-h-8 flex-wrap items-center gap-3">
				<span className="w-[80px] flex-none text-[length:var(--text-caption,12px)] font-medium text-[var(--fg)]">
					File
				</span>
				<code className="min-w-0 flex-1 truncate font-mono text-[11px] text-[var(--fg-muted)]" title={path}>
					{path}
				</code>
			</div>
			<div className="flex min-h-8 flex-wrap items-center gap-3">
				<label
					htmlFor={ids.expires}
					className="w-[80px] flex-none text-[length:var(--text-caption,12px)] font-medium text-[var(--fg)]"
				>
					Expires
				</label>
				<select
					id={ids.expires}
					value={days === null ? 'never' : String(days)}
					onChange={(e) => setDays(e.target.value === 'never' ? null : Number(e.target.value))}
					className={FIELD}
				>
					{LINK_EXPIRY_CHOICES.map((c) => (
						<option key={c.label} value={c.days === null ? 'never' : String(c.days)}>
							{c.label}
						</option>
					))}
				</select>
			</div>
			<div className="flex min-h-8 flex-wrap items-center gap-3">
				<label
					htmlFor={ids.passcode}
					className="w-[80px] flex-none text-[length:var(--text-caption,12px)] font-medium text-[var(--fg)]"
				>
					Passcode
				</label>
				<input
					id={ids.passcode}
					type="password"
					autoComplete="new-password"
					value={passcode}
					onChange={(e) => setPasscode(e.target.value)}
					placeholder="Optional, 6 or more characters"
					className={cn(FIELD, 'w-full max-w-[320px]')}
				/>
			</div>
			{shownProblem && (
				<p className="m-0 text-[length:var(--text-micro)] text-[var(--danger)]" data-passcode-problem>
					{shownProblem}
				</p>
			)}
			{error && (
				<p role="alert" className="m-0 text-[12px] text-[var(--danger)]">
					{error}
				</p>
			)}
			<div className="flex justify-end">
				<span title={blocker ?? undefined}>
					<Button
						type="button"
						size="sm"
						disabled={busy || blocker !== null}
						onClick={() => void create()}
					>
						Create link
					</Button>
				</span>
			</div>
		</div>
	);
}
