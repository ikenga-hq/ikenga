// The onboarding "offline engine" install can fail at any step — kernel
// status, the registry index/detail reads, or the tarball install itself.
// Name the actual cause instead of blaming the registry for every failure.

import { classifyInstallError, errorText } from '@/lib/ngwa/install-errors';

const LATER = 'You can also install the offline engine later from Ngwa → Store.';

export function offlineInstallErrorMessage(error: unknown): string {
	const raw = errorText(error);
	// The registry index is signature-checked before anything is installed;
	// a failed check is not a connectivity problem.
	if (/\bsignature\b/i.test(raw) && !/integrity/i.test(raw)) {
		return `The registry index couldn't be verified (signature check failed), so nothing was installed. ${LATER}`;
	}
	const { message } = classifyInstallError(error, 'the offline engine');
	return `${message} ${LATER}`;
}
