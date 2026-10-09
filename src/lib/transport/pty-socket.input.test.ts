// Typing over the PTY socket (`sendPtyInput`): in order, binary, only while
// the socket is open, and nowhere once the attachment is gone.

import { describe, expect, it, vi } from 'vitest';
import { attachRemotePty, sendPtyInput } from './pty-socket';

vi.mock('./reauth-store', () => ({
	useReauthStore: { getState: () => ({ showReauth: vi.fn() }) },
}));

class FakeSocket {
	onopen: (() => void) | null = null;
	onclose: ((e?: { code: number; reason: string }) => void) | null = null;
	onerror: ((e: unknown) => void) | null = null;
	onmessage: ((e: { data: unknown }) => void) | null = null;
	readyState: number = WebSocket.CONNECTING;
	sent: unknown[] = [];
	send(data: unknown) {
		this.sent.push(data);
	}
	close() {
		this.readyState = WebSocket.CLOSED;
	}
	open() {
		this.readyState = WebSocket.OPEN;
		this.onopen?.();
	}
}

function attach(id: string) {
	const sockets: FakeSocket[] = [];
	const detach = attachRemotePty(
		() => {
			const s = new FakeSocket();
			sockets.push(s);
			return s as unknown as WebSocket;
		},
		id,
		() => {},
		() => {}
	);
	return { sockets, detach };
}

const text = (frames: unknown[]) =>
	frames.map((f) => new TextDecoder().decode(f as Uint8Array)).join('');

describe('sendPtyInput', () => {
	it('is refused until the socket opens, then sends binary frames in order', () => {
		const { sockets, detach } = attach('t1');
		expect(sendPtyInput('t1', 'x')).toBe(false);
		sockets[0].open();
		for (const k of ['t', 'h', 'e', '\r']) expect(sendPtyInput('t1', k)).toBe(true);
		expect(sockets[0].sent.every((f) => ArrayBuffer.isView(f))).toBe(true);
		expect(text(sockets[0].sent)).toBe('the\r');
		detach();
	});

	it('never sends text frames, so typed JSON cannot become a control message', () => {
		const { sockets, detach } = attach('t2');
		sockets[0].open();
		sendPtyInput('t2', '{"type":"kill"}');
		expect(typeof sockets[0].sent[0]).not.toBe('string');
		detach();
	});

	it('is refused while reconnecting and after detach', () => {
		vi.useFakeTimers();
		const { sockets, detach } = attach('t3');
		sockets[0].open();
		sockets[0].readyState = WebSocket.CLOSED;
		sockets[0].onclose?.();
		expect(sendPtyInput('t3', 'a')).toBe(false);
		vi.advanceTimersByTime(1000);
		sockets[1].open();
		expect(sendPtyInput('t3', 'a')).toBe(true);
		detach();
		expect(sendPtyInput('t3', 'a')).toBe(false);
		vi.useRealTimers();
	});

	it('a view-only device: refusals are not painted, and typing leaves the socket', () => {
		const chunks: string[] = [];
		const sockets: FakeSocket[] = [];
		const detach = attachRemotePty(
			() => {
				const s = new FakeSocket();
				sockets.push(s);
				return s as unknown as WebSocket;
			},
			't5',
			(bytes) => chunks.push(new TextDecoder().decode(bytes)),
			() => {}
		);
		sockets[0].open();
		expect(sendPtyInput('t5', 'l')).toBe(true);
		const refusal = JSON.stringify({ type: 'error', code: 'forbidden', missing: ['dispatch'] });
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
		sockets[0].onmessage?.({ data: refusal });
		expect(chunks.join('')).toBe('');
		expect(sendPtyInput('t5', 's')).toBe(false); // falls back to the RPC
		warn.mockRestore();
		detach();
	});

	it('does not route to another PTY', () => {
		const { sockets, detach } = attach('t4');
		sockets[0].open();
		expect(sendPtyInput('other', 'a')).toBe(false);
		expect(sockets[0].sent).toHaveLength(0);
		detach();
	});
});
