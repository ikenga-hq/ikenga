// plans/pwa S1 (W2): the browser's own online/offline signal, for copy that
// tells "this device is offline" apart from "the server went away".

import { useSyncExternalStore } from 'react';

function subscribe(onChange: () => void): () => void {
	window.addEventListener('online', onChange);
	window.addEventListener('offline', onChange);
	return () => {
		window.removeEventListener('online', onChange);
		window.removeEventListener('offline', onChange);
	};
}

function snapshot(): boolean {
	return typeof navigator === 'undefined' || navigator.onLine !== false;
}

/** False only when the browser positively reports being offline. */
export function useOnline(): boolean {
	return useSyncExternalStore(subscribe, snapshot, () => true);
}
