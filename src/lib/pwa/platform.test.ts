// plans/pwa Shape 4 (install part): the install guidance says what this
// browser can actually do.

import { describe, expect, it } from 'vitest';
import {
	installMessage,
	installState,
	isIos,
	type PlatformEnv,
	tokenSessionWarning,
} from './platform';

const CHROME_ANDROID =
	'Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0 Mobile Safari/537.36';
const IPHONE =
	'Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1';
const IPAD_DESKTOP_UA =
	'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Safari/605.1.15';

function env(over: Partial<PlatformEnv> = {}): PlatformEnv {
	return {
		isTauri: false,
		secureContext: true,
		standalone: false,
		userAgent: CHROME_ANDROID,
		platform: 'Linux armv8l',
		maxTouchPoints: 5,
		hasInstallPrompt: false,
		...over,
	};
}

describe('installState', () => {
	it('says nothing on the desktop app or once installed', () => {
		expect(installState(env({ isTauri: true }))).toBe('tauri');
		expect(installState(env({ standalone: true }))).toBe('installed');
		expect(installMessage('tauri')).toBeNull();
		expect(installMessage('installed')).toBeNull();
	});

	it('names HTTPS as the blocker on a plain-HTTP origin, before any platform advice', () => {
		expect(installState(env({ secureContext: false, userAgent: IPHONE }))).toBe('insecure');
		expect(installMessage('insecure')).toMatch(/HTTPS/);
	});

	it('gives iOS the Share-sheet steps and the pair-inside-the-app caveat', () => {
		expect(installState(env({ userAgent: IPHONE, platform: 'iPhone' }))).toBe('ios-manual');
		expect(
			installState(env({ userAgent: IPAD_DESKTOP_UA, platform: 'MacIntel', maxTouchPoints: 5 }))
		).toBe('ios-manual');
		expect(installMessage('ios-manual')).toMatch(/Add to Home Screen/);
		expect(installMessage('ios-manual')).toMatch(/pair/);
	});

	it('offers a button only when the browser handed over a prompt', () => {
		expect(installState(env({ hasInstallPrompt: true }))).toBe('prompt');
		expect(installState(env())).toBe('browser-menu');
	});

	it('does not mistake a real Mac for an iPad', () => {
		expect(isIos({ userAgent: IPAD_DESKTOP_UA, platform: 'MacIntel', maxTouchPoints: 0 })).toBe(
			false
		);
	});
});

describe('tokenSessionWarning', () => {
	const base = { standalone: true, hasBearerToken: true, isDevice: false, isT1: false };
	it('warns an installed app running on a T0 link token', () => {
		expect(tokenSessionWarning(base)).toMatch(/Pair this device/);
	});
	it('is silent for a tab, a paired device, a T1 session or no token', () => {
		expect(tokenSessionWarning({ ...base, standalone: false })).toBeNull();
		expect(tokenSessionWarning({ ...base, isDevice: true })).toBeNull();
		expect(tokenSessionWarning({ ...base, isT1: true })).toBeNull();
		expect(tokenSessionWarning({ ...base, hasBearerToken: false })).toBeNull();
	});
});
