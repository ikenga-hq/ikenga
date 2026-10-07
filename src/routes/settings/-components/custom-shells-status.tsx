import { AlertTriangle } from 'lucide-react';
import { Banner } from '@/components/ui/banner';
import { Button } from '@/components/ui/button';

/**
 * Settings > Engines: why the custom-shells list can't be edited, and (only
 * for a corrupt value, not a temporary read failure) "Reset custom shells",
 * which backs the value up to a side setting before clearing it (D-13).
 */
export function CustomShellsStatus({
	error,
	isCorrupt,
	onReset,
	onRetry,
	isResetting,
	resetError,
	resetBackupKey,
}: {
	error: Error | null;
	isCorrupt: boolean;
	onReset: () => Promise<unknown>;
	onRetry: () => void;
	isResetting: boolean;
	resetError: Error | null;
	resetBackupKey: string | null;
}) {
	return (
		<>
			{error && (
				<div className="border-t border-border px-4 py-3">
					<Banner
						tone="danger"
						icon={<AlertTriangle />}
						role="alert"
						className="rounded-md border"
						data-testid="custom-shells-read-error"
						actions={
							<>
								{isCorrupt && (
									<Button
										variant="ghost"
										size="sm"
										disabled={isResetting}
										data-testid="custom-shells-reset"
										onClick={() => void onReset().catch(() => {})}
									>
										Reset custom shells
									</Button>
								)}
								<Button variant="ghost" size="sm" onClick={onRetry}>
									Retry
								</Button>
							</>
						}
					>
						<div className="text-[13px] font-semibold">Couldn't read your custom shells</div>
						<div className="mt-1 text-xs" style={{ color: 'var(--fg-muted)' }}>
							{error instanceof Error ? error.message : String(error)}. Adding or removing custom
							shells is paused so the saved list isn't overwritten.
							{isCorrupt &&
								' Reset custom shells saves a copy of the current value under a backup setting, then starts an empty list.'}
						</div>
						{resetError && (
							<div className="mt-1 text-xs" style={{ color: 'var(--danger)' }}>
								Couldn't reset (
								{resetError instanceof Error ? resetError.message : String(resetError)}
								). Your custom shells were not cleared.
							</div>
						)}
					</Banner>
				</div>
			)}

			{!error && resetBackupKey && (
				<div
					className="border-t border-border px-4 py-3 text-xs"
					style={{ color: 'var(--fg-muted)' }}
					role="status"
					data-testid="custom-shells-reset-done"
				>
					Custom shells were reset. The unreadable value was saved as the setting{' '}
					<code>{resetBackupKey}</code>.
				</div>
			)}
		</>
	);
}
