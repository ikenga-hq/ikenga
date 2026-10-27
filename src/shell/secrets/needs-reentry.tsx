import { TriangleAlert } from 'lucide-react';
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
import {
	type QuarantinedSecret,
	useDeleteQuarantinedSecret,
	useSetQuarantinedSecret,
} from '@/lib/queries/secrets';
import { confirm as confirmDialog } from '@/lib/transport/dialog-shim';

/** Where a secret lives, in plain words. */
export function quarantinedPlace(entry: QuarantinedSecret): string {
	if (!entry.scope) return 'Older entry';
	if (entry.scope.kind === 'workspace') return 'Personal';
	if (entry.scope.kind === 'project') return `Project ${entry.scope.id}`;
	return `App or extension ${entry.scope.id}`;
}

/**
 * Secrets that could not be moved into the OS keychain when the vault was
 * upgraded. Their old copy is still safe in the backup, but the app cannot use
 * them until they are entered again (or removed). The rest of the vault works
 * normally. Only names are shown; nothing here ever reads a value.
 */
export function NeedsReentryGroup({
	entries,
	writable,
}: {
	entries: QuarantinedSecret[];
	/** The vault is unlocked and can be written to. */
	writable: boolean;
}) {
	const [editing, setEditing] = useState<QuarantinedSecret | null>(null);
	const delMut = useDeleteQuarantinedSecret();
	if (entries.length === 0) return null;

	return (
		<section
			data-testid="needs-reentry"
			aria-label="Needs re-entry"
			className="overflow-hidden rounded-lg border border-amber-200 bg-amber-50 text-amber-950 dark:border-amber-900/40 dark:bg-amber-950/30 dark:text-amber-100"
		>
			<div className="flex items-start gap-2 px-3 py-2.5 text-xs">
				<TriangleAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" />
				<div className="space-y-1">
					<h3 className="text-sm font-semibold">Needs re-entry</h3>
					<p className="leading-relaxed">
						{entries.length === 1 ? 'This secret' : 'These secrets'} could not be moved into your
						keychain when the vault was upgraded, so the app cannot use{' '}
						{entries.length === 1 ? 'it' : 'them'} yet. Type the value in again, or remove it if you
						no longer need it. Everything else in your vault works as normal.
					</p>
				</div>
			</div>
			<ul className="divide-y divide-amber-200 border-t border-amber-200 dark:divide-amber-900/40 dark:border-amber-900/40">
				{entries.map((entry) => (
					<li
						key={entry.name}
						className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-3 px-3 py-2 text-xs"
					>
						<div className="min-w-0">
							<span className="block truncate font-mono font-medium">{entry.key}</span>
							<span className="text-[10px] opacity-80">{quarantinedPlace(entry)}</span>
						</div>
						<div className="flex items-center gap-1">
							<Button
								variant="outline"
								size="sm"
								className="h-6 px-2 text-[11px]"
								disabled={!writable}
								title={writable ? undefined : 'Unlock the vault first'}
								onClick={() => setEditing(entry)}
								aria-label={`Enter ${entry.key} again`}
							>
								Enter again
							</Button>
							<Button
								variant="ghost"
								size="sm"
								className="h-6 px-2 text-[11px]"
								disabled={!writable || delMut.isPending}
								title={writable ? undefined : 'Unlock the vault first'}
								aria-label={`Remove ${entry.key}`}
								onClick={async () => {
									const ok = await confirmDialog(
										`Remove "${entry.key}"? Anything using it will stop working until you add it again.`,
										{ title: 'Remove secret', kind: 'warning' }
									);
									if (ok) delMut.mutate(entry);
								}}
							>
								Remove
							</Button>
						</div>
					</li>
				))}
			</ul>
			{delMut.error && (
				<p role="alert" className="border-t border-amber-200 px-3 py-2 text-xs text-red-700">
					{delMut.error.message}
				</p>
			)}
			{editing && <ReentryDialog entry={editing} onClose={() => setEditing(null)} />}
		</section>
	);
}

function ReentryDialog({ entry, onClose }: { entry: QuarantinedSecret; onClose: () => void }) {
	const [value, setValue] = useState('');
	const setMut = useSetQuarantinedSecret();
	return (
		<Dialog open onOpenChange={(open) => !open && onClose()}>
			<DialogContent>
				<DialogHeader>
					<DialogTitle>Enter {entry.key} again</DialogTitle>
					<DialogDescription>
						{quarantinedPlace(entry)}. The value is stored in your OS keychain and checked right
						after it is saved. It is never written to a log.
					</DialogDescription>
				</DialogHeader>
				<div>
					<label className="text-xs font-medium" htmlFor="reentry-value-input">
						Value
					</label>
					<Input
						id="reentry-value-input"
						type="password"
						value={value}
						onChange={(e) => setValue(e.target.value)}
						className="font-mono"
					/>
				</div>
				{setMut.error && (
					<p role="alert" className="text-xs text-red-700">
						{setMut.error.message}
					</p>
				)}
				<DialogFooter>
					<Button variant="ghost" onClick={onClose}>
						Cancel
					</Button>
					<Button
						disabled={value.length === 0 || setMut.isPending}
						onClick={() => setMut.mutate({ entry, value }, { onSuccess: onClose })}
					>
						Save
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}
