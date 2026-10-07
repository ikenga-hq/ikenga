// The onboarding "offline engine" install can fail at any step — kernel
// status, the registry index/detail reads, or the tarball install itself.
// Name the actual cause instead of blaming the registry for every failure.

import { classifyInstallError, errorText, installErrorMessage } from '@/lib/ngwa/install-errors';

const LATER = 'You can also install the offline engine later from Ngwa → Store.';
const NAME = 'the offline engine';

/** The webview's own fetch() (registry index / detail reads) rejects with a
 *  bare TypeError naming no cause. Kept local: the shared classifier serves
 *  Rust/npm install output, where these strings don't appear. */
const WEBVIEW_FETCH_FAILURE = /^(TypeError: )?(Failed to fetch|NetworkError when attempting to fetch resource\.?|Load failed)$/i;

export function offlineInstallErrorMessage(error: unknown): string {
	const raw = errorText(error);
	// The registry index is signature-checked before anything is installed;
	// a failed check is not a connectivity problem.
	if (/\bsignature\b/i.test(raw) && !/integrity/i.test(raw)) {
		return `The registry index couldn't be verified (signature check failed), so nothing was installed. ${LATER}`;
	}
	if (WEBVIEW_FETCH_FAILURE.test(raw.trim())) {
		return `${installErrorMessage('network', NAME)} ${LATER}`;
	}
	const { message } = classifyInstallError(error, NAME);
	return `${message} ${LATER}`;
}
