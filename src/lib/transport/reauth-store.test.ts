import { beforeEach, describe, expect, it } from 'vitest';
import { useReauthStore } from './reauth-store';

describe('reauth store reason', () => {
	beforeEach(() => {
		useReauthStore.getState().hideReauth();
		useReauthStore.setState({ reason: 'expired' });
	});

	it('defaults to "expired", the original 401 behaviour', () => {
		useReauthStore.getState().showReauth();
		expect(useReauthStore.getState()).toMatchObject({ isOpen: true, reason: 'expired' });
	});

	it('records a first visit so the copy does not claim a token expired', () => {
		useReauthStore.getState().showReauth('first-visit');
		expect(useReauthStore.getState()).toMatchObject({ isOpen: true, reason: 'first-visit' });
	});

	it('clears a previous error when reopened', () => {
		useReauthStore.getState().setErrorMsg('nope');
		useReauthStore.getState().showReauth('first-visit');
		expect(useReauthStore.getState().errorMsg).toBeNull();
	});
});
