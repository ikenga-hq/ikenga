// D-06 shared part (WP-57, review round 1 blocker 5): the banner shown above
// the Actions list for per-file `validation.errors` (§1.1) and model-level
// `issues`, plus a soft notice when the last read from disk failed (the
// model shown is the last good one — never replaced by `ErrorState`, so the
// surface the user is working in never disappears out from under them).

import type { EffectiveModel } from '@/lib/actions/store';

interface IssueLine {
	key: string;
	message: string;
}

function fileIssues(model: EffectiveModel): IssueLine[] {
	const files = model.files;
	if (!files) return [];
	const out: IssueLine[] = [];
	const scoped = [files.personal, files.project].filter((f): f is NonNullable<typeof f> => f !== null);
	for (const scope of scoped) {
		for (const kind of ['actions', 'keybindings'] as const) {
			const state = scope[kind];
			for (const err of state.validation.errors) {
				out.push({
					key: `${scope.scope}:${kind}:${err.path}:${err.code}`,
					message: `${scope.scope} ${kind} (${state.path}): ${err.message}`,
				});
			}
		}
	}
	return out;
}

function modelIssues(model: EffectiveModel): IssueLine[] {
	return model.issues.map((issue, i) => ({
		key: `issue:${i}:${issue.scope}:${issue.file}:${issue.code}`,
		message: `${issue.scope} ${issue.file}: ${issue.message}`,
	}));
}

export interface IssuesBannerProps {
	model: EffectiveModel;
	/** The store's last read failure, if any — the model shown is still the
	 *  last good one (§1.1: never wiped by a bad read). */
	readError: string | null;
}

export function IssuesBanner({ model, readError }: IssuesBannerProps) {
	const lines = [...fileIssues(model), ...modelIssues(model)];
	if (lines.length === 0 && !readError) return null;
	return (
		<div className="issuesbanner" role="alert">
			{readError && (
				<div className="issuesbanner-row">
					Couldn't re-read the actions files from disk — showing the last good read. {readError}
				</div>
			)}
			{lines.map((line) => (
				<div key={line.key} className="issuesbanner-row">
					{line.message}
				</div>
			))}
		</div>
	);
}
