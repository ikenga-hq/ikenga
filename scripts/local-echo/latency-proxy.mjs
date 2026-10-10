#!/usr/bin/env node
/**
 * TCP proxy that adds round-trip latency with jitter, for exercising
 * predictive local echo against a local `ikenga-server`.
 *
 *   node scripts/local-echo/latency-proxy.mjs --listen 4100 --target 4000 --rtt 300 --jitter 80
 *
 * Every chunk in each direction is held for (rtt/2 ± jitter/2) ms, uniformly
 * drawn, so a request + response sees rtt ± jitter. Order is kept within a
 * connection (a chunk is never released before the one ahead of it) — as on a
 * real TCP path — but separate connections are independent, so the browser's
 * parallel HTTP requests can still overtake each other, as they can on a real
 * mobile link. Works for HTTP and WebSocket alike: it never parses a byte.
 *
 * `--control 4101` also serves `GET /?rtt=300&jitter=80` on that port to
 * change the latency of every connection, open ones included — so a page can
 * be loaded fast and then typed into slowly.
 *
 * Not a test: a dev tool for `run-live.mjs`.
 */

import http from 'node:http';
import net from 'node:net';

function arg(name, fallback) {
	const i = process.argv.indexOf(`--${name}`);
	return i >= 0 ? process.argv[i + 1] : fallback;
}

const listen = Number(arg('listen', '4100'));
const target = Number(arg('target', '4000'));
const targetHost = arg('target-host', '127.0.0.1');
let rtt = Number(arg('rtt', '300'));
let jitter = Number(arg('jitter', '80'));
const control = arg('control', null);

const oneWay = () => rtt / 2 + (Math.random() * 2 - 1) * (jitter / 2);

/** Forward `from` → `to`, delaying each chunk but never reordering them:
 *  one FIFO per direction, drained by a single timer. */
function pipeDelayed(from, to) {
	const queue = [];
	let ended = false;
	let timer = null;
	const drain = () => {
		timer = null;
		const now = Date.now();
		while (queue.length > 0 && queue[0].at <= now) {
			const { chunk } = queue.shift();
			if (!to.destroyed) to.write(chunk);
		}
		if (queue.length > 0) timer = setTimeout(drain, queue[0].at - now);
		else if (ended) to.end();
	};
	from.on('data', (chunk) => {
		const last = queue.length > 0 ? queue[queue.length - 1].at : 0;
		queue.push({ chunk, at: Math.max(last, Date.now() + oneWay()) });
		if (!timer) timer = setTimeout(drain, queue[0].at - Date.now());
	});
	from.on('end', () => {
		ended = true;
		if (!timer && queue.length === 0) to.end();
	});
	from.on('error', () => to.destroy());
}

const server = net.createServer((client) => {
	client.setNoDelay(true);
	const upstream = net.connect(target, targetHost);
	upstream.setNoDelay(true);
	pipeDelayed(client, upstream);
	pipeDelayed(upstream, client);
	// Half-closes travel through `pipeDelayed` (after the queued chunks);
	// only a hard error tears the pair down at once.
	upstream.on('error', () => client.destroy());
	client.on('error', () => upstream.destroy());
});

server.listen(listen, '127.0.0.1', () => {
	console.log(
		`latency-proxy 127.0.0.1:${listen} → ${targetHost}:${target}  rtt ${rtt}±${jitter} ms`
	);
});

if (control) {
	http
		.createServer((req, res) => {
			const q = new URL(req.url ?? '/', 'http://x').searchParams;
			if (q.has('rtt')) rtt = Number(q.get('rtt'));
			if (q.has('jitter')) jitter = Number(q.get('jitter'));
			console.log(`latency now rtt ${rtt}±${jitter} ms`);
			res.end(JSON.stringify({ rtt, jitter }));
		})
		.listen(Number(control), '127.0.0.1');
}
