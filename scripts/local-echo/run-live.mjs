#!/usr/bin/env node
/**
 * Live check of predictive local echo against a running `ikenga-server`,
 * usually through `latency-proxy.mjs`. Drives the real SPA in Chromium:
 * opens a bash terminal and types through xterm's own textarea, the way a
 * person does.
 *
 *   node scripts/local-echo/run-live.mjs --url http://127.0.0.1:4100 \
 *     --token $TOKEN --mode auto --out /tmp/le-auto [--claude] [--flaky path/to/flaky_echo.py]
 *
 * Per scenario it records, for every keystroke, when the character became
 * visible (overlay or buffer) and when the server's echo landed in the buffer,
 * sampled every animation frame. It also checks that no prediction is left on
 * screen at the end, and writes the terminal's whole buffer to
 * `<out>/buffer.txt` so a run with `--mode off` can be diffed against it.
 *
 * Needs `localStorage['ikenga.debug.localEcho'] = '1'` (set here) to reach the
 * xterm through `window.__ikengaLocalEcho`. Not part of CI.
 */

import fs from 'node:fs';
import path from 'node:path';
import { chromium } from '@playwright/test';

function arg(name, fallback) {
	const i = process.argv.indexOf(`--${name}`);
	if (i < 0) return fallback;
	const v = process.argv[i + 1];
	return v === undefined || v.startsWith('--') ? true : v;
}

const url = arg('url', 'http://127.0.0.1:4100');
const token = arg('token');
const mode = arg('mode', 'auto');
const out = arg('out', `/tmp/local-echo-${mode}`);
const withClaude = arg('claude', false) === true;
const flaky = arg('flaky', null);
const secondViewer = arg('second-viewer', false) === true;
const keyDelay = Number(arg('key-delay', '110'));
/** latency-proxy control port: the page loads at no added latency (opening a
 *  terminal over a 300 ms link currently times out — a separate issue), then
 *  the link is slowed to `--rtt`±`--jitter` before any typing. */
const control = arg('control', null);
const rtt = arg('rtt', '300');
const jitter = arg('jitter', '80');
async function setLatency(r, j) {
	if (!control) return;
	await fetch(`http://127.0.0.1:${control}/?rtt=${r}&jitter=${j}`);
}
fs.mkdirSync(out, { recursive: true });

const browser = await chromium.launch({
	executablePath: process.env.CHROMIUM_PATH ?? '/opt/pw-browsers/chromium',
});
const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
await page.addInitScript((m) => {
	localStorage.setItem('ikenga.debug.localEcho', '1');
	localStorage.setItem('ikenga.terminal.localEcho', m);
}, mode);
await setLatency(0, 0);
await page.goto(`${url}/?token=${token}`);
await page.waitForTimeout(4000);
await page.mouse.click(700, 400);
await page.keyboard.press('Alt+t'); // new bash terminal tab
await page.waitForFunction(() => (window.__ikengaLocalEcho?.size ?? 0) > 0, null, {
	timeout: 20_000,
});
await page.waitForTimeout(2500);

// In-page helpers: the newest terminal, its cursor row, and the overlay.
await page.evaluate(() => {
	const w = window;
	w.__le = {
		entry: () => [...w.__ikengaLocalEcho].at(-1),
		term: () => w.__le.entry().term,
		cursorRow: () => {
			const b = w.__le.term().buffer.active;
			return { y: b.baseY + b.cursorY, x: b.cursorX };
		},
		rowText: (y) => w.__le.term().buffer.active.getLine(y)?.translateToString(false) ?? '',
		overlay: () => {
			const cells = [...document.querySelectorAll('.ikenga-local-echo span')].map((s) => ({
				x: Number(s.dataset.x),
				ch: s.textContent,
				predicted: s.hasAttribute('data-predicted'),
			}));
			return cells;
		},
		/** The row as the user sees it: buffer text with overlay cells on top. */
		composite: (y) => {
			const row = w.__le.rowText(y).split('');
			for (const c of w.__le.overlay()) row[c.x] = c.ch || ' ';
			return row.join('');
		},
		dumpBuffer: () => {
			const b = w.__le.term().buffer.normal;
			const lines = [];
			for (let i = 0; i < b.length; i++) lines.push(b.getLine(i)?.translateToString(true) ?? '');
			while (lines.length && lines.at(-1) === '') lines.pop();
			return lines.join('\n');
		},
		stats: () => w.__le.entry().stats(),
	};
});

await page.locator('.xterm').first().click();
await setLatency(rtt, jitter);

async function waitQuiet(ms = 800) {
	await page.waitForTimeout(ms);
}

async function dumpFailure(why) {
	console.error(`[${mode}] FAILED: ${why}`);
	await page.screenshot({ path: path.join(out, 'failure.png') }).catch(() => {});
	const dump = await page.evaluate(() => window.__le.dumpBuffer()).catch(() => '');
	fs.writeFileSync(path.join(out, 'failure-buffer.txt'), dump);
}

/** Wait for the cursor row to start with `text` — a prompt the server drew,
 *  not the command line that merely mentions it. */
async function waitForRowContaining(text, timeout = 10_000) {
	await page
		.waitForFunction(
			(t) => {
				const { y } = window.__le.cursorRow();
				return window.__le.rowText(y).startsWith(t);
			},
			text,
			{ timeout }
		)
		.catch(async (err) => {
			await dumpFailure(`row never showed ${JSON.stringify(text)}`);
			throw err;
		});
}

/**
 * Type `text` key by key and time every key: keydown → visible (composite
 * row shows the prefix) and keydown → echoed (the buffer itself shows it).
 */
async function measureTyping(label, text, { delay = keyDelay, screenshotAt = null } = {}) {
	await page.evaluate(() => {
		const w = window;
		const { y, x } = w.__le.cursorRow();
		w.__m = { y, x, keys: [], samples: [], maxPredicted: 0, run: true };
		const onKey = (e) => {
			if (e.key.length === 1) w.__m.keys.push(performance.now());
		};
		document.addEventListener('keydown', onKey, true);
		w.__m.stop = () => document.removeEventListener('keydown', onKey, true);
		const tick = () => {
			if (!w.__m.run) return;
			const m = w.__m;
			const now = performance.now();
			const ov = w.__le.overlay();
			m.maxPredicted = Math.max(m.maxPredicted, ov.filter((c) => c.predicted).length);
			m.samples.push({
				t: now,
				buf: w.__le.rowText(m.y).slice(m.x),
				shown: w.__le.composite(m.y).slice(m.x),
			});
			requestAnimationFrame(tick);
		};
		requestAnimationFrame(tick);
	});
	for (let i = 0; i < text.length; i++) {
		await page.keyboard.type(text[i]);
		if (screenshotAt !== null && i === screenshotAt) {
			await page.screenshot({ path: path.join(out, `${label}-mid.png`) });
		}
		await page.waitForTimeout(delay);
	}
	await page
		.waitForFunction(
			(t) => window.__le.rowText(window.__m.y).slice(window.__m.x).startsWith(t),
			text,
			{ timeout: 10_000 }
		)
		.catch(() => {});
	await waitQuiet(1200);
	const res = await page.evaluate((t) => {
		const m = window.__m;
		m.run = false;
		m.stop();
		const firstAt = (pred) => m.samples.find(pred)?.t ?? null;
		const perKey = [];
		for (let i = 0; i < t.length; i++) {
			const prefix = t.slice(0, i + 1);
			const at = m.keys[i];
			const shown = firstAt((s) => s.t >= at && s.shown.startsWith(prefix));
			const echoed = firstAt((s) => s.t >= at && s.buf.startsWith(prefix));
			perKey.push({
				key: t[i],
				visibleMs: shown === null ? null : Math.round(shown - at),
				echoMs: echoed === null ? null : Math.round(echoed - at),
			});
		}
		return {
			perKey,
			maxPredicted: m.maxPredicted,
			finalRow: window.__le.rowText(m.y).trimEnd(),
			overlayLeft: window.__le.overlay().length,
		};
	}, text);
	const vis = res.perKey.map((k) => k.visibleMs).filter((v) => v !== null);
	const echo = res.perKey.map((k) => k.echoMs).filter((v) => v !== null);
	const med = (a) => (a.length ? [...a].sort((p, q) => p - q)[Math.floor(a.length / 2)] : null);
	const summary = {
		label,
		text,
		visibleMedianMs: med(vis),
		visibleMaxMs: vis.length ? Math.max(...vis) : null,
		echoMedianMs: med(echo),
		keysVisibleUnder50ms: vis.filter((v) => v < 50).length,
		keys: text.length,
		...res,
	};
	console.log(
		`[${mode}] ${label}: visible median ${summary.visibleMedianMs} ms (max ${summary.visibleMaxMs}), ` +
			`echo median ${summary.echoMedianMs} ms, ${summary.keysVisibleUnder50ms}/${text.length} keys visible <50 ms, ` +
			`max predicted cells ${res.maxPredicted}, overlay left ${res.overlayLeft}`
	);
	return summary;
}

async function enter() {
	await page.keyboard.press('Enter');
}

const results = { mode, scenarios: [] };

// 1. Plain typing at the bash prompt.
await waitQuiet(1500);
results.scenarios.push(
	await measureTyping('bash', 'echo predictive-local-echo', { screenshotAt: 14 })
);
await enter();
await waitQuiet(1500);

// 2. Editing inside the predicted region: Left, insert, Right, Backspace.
await page.evaluate(() => {
	window.__edit = { maxPredicted: 0, run: true };
	const tick = () => {
		if (!window.__edit.run) return;
		window.__edit.maxPredicted = Math.max(
			window.__edit.maxPredicted,
			window.__le.overlay().filter((c) => c.predicted).length
		);
		requestAnimationFrame(tick);
	};
	requestAnimationFrame(tick);
});
const editKeys = [...'echo helo', 'ArrowLeft', 'l', 'ArrowRight', ...' worldd', 'Backspace'];
for (const k of editKeys) {
	await page.keyboard.press(k === ' ' ? 'Space' : k);
	await page.waitForTimeout(keyDelay);
}
await page.screenshot({ path: path.join(out, 'edit-before-enter.png') });
await waitQuiet(1500);
const editRow = await page.evaluate(() => {
	window.__edit.run = false;
	const { y } = window.__le.cursorRow();
	return {
		row: window.__le.rowText(y).trimEnd(),
		overlayLeft: window.__le.overlay().length,
		maxPredicted: window.__edit.maxPredicted,
	};
});
console.log(
	`[${mode}] edit: row ${JSON.stringify(editRow.row)}, overlay left ${editRow.overlayLeft}`
);
results.scenarios.push({ label: 'edit', ...editRow });
await enter();
await waitQuiet(1500);

// 3. Echo off: a password read. Nothing typed there may ever be painted.
// biome-ignore lint/suspicious/noTemplateCurlyInString: bash parameter expansion, typed literally
await page.keyboard.type('read -s -p "Password: " pw; echo; echo "len=${#pw}"', {
	delay: 20,
});
await enter();
await waitForRowContaining('Password:');
results.scenarios.push(await measureTyping('password', 'hunter22'));
await enter();
await waitQuiet(1500);

// 4. A line editor that stops echoing what was typed: forced mispredictions.
if (typeof flaky === 'string') {
	await page.keyboard.type(`python3 ${flaky}`, { delay: 20 });
	await enter();
	await waitForRowContaining('flaky>');
	await waitQuiet(500);
	results.scenarios.push(await measureTyping('flaky', 'abcdefghijkl', { delay: 250 }));
	await page.screenshot({ path: path.join(out, 'flaky-after.png') });
	await enter();
	await waitQuiet(1500);
}

// 5. Bracketed paste, through xterm's own paste path.
await page.evaluate(() => window.__le.term().paste('echo pasted-text'));
await waitQuiet(1500);
const pasteState = await page.evaluate(() => ({
	overlayLeft: window.__le.overlay().length,
	stats: window.__le.stats(),
}));
results.scenarios.push({ label: 'paste', ...pasteState });
await enter();
await waitQuiet(1500);

// 6. A second viewer of the same PTY (another tab attached to the session):
// it must never paint a prediction for keys typed in the first, and both must
// end on the same screen.
if (secondViewer) {
	const page2 = await browser.newPage({ viewport: { width: 1280, height: 800 } });
	await page2.addInitScript((m) => {
		localStorage.setItem('ikenga.debug.localEcho', '1');
		localStorage.setItem('ikenga.terminal.localEcho', m);
	}, mode);
	await setLatency(0, 0);
	await page2.goto(`${url}/?token=${token}`);
	await page2.waitForTimeout(4000);
	await page2.getByRole('button', { name: /^bash/ }).first().click();
	await page2.waitForFunction(() => (window.__ikengaLocalEcho?.size ?? 0) > 0, null, {
		timeout: 20_000,
	});
	await setLatency(rtt, jitter);
	await page2.waitForTimeout(1500);
	await page2.evaluate(() => {
		const w = window;
		w.__v2 = { maxCells: 0, run: true };
		const tick = () => {
			if (!w.__v2.run) return;
			w.__v2.maxCells = Math.max(
				w.__v2.maxCells,
				document.querySelectorAll('.ikenga-local-echo span').length
			);
			requestAnimationFrame(tick);
		};
		requestAnimationFrame(tick);
	});
	await page.bringToFront();
	await page.locator('.xterm').first().click();
	const typed = await measureTyping('viewer-1', 'echo two-viewers');
	await page2.screenshot({ path: path.join(out, 'viewer-2.png') });
	await enter();
	await waitQuiet(2000);
	const lastRows = (p) =>
		p.evaluate(() => {
			const t = [...window.__ikengaLocalEcho].at(-1).term;
			const b = t.buffer.active;
			const rows = [];
			for (let i = Math.max(0, b.length - 4); i < b.length; i++)
				rows.push(b.getLine(i)?.translateToString(true) ?? '');
			return rows.join('\n');
		});
	const v1 = await lastRows(page);
	const v2 = await lastRows(page2);
	const v2Max = await page2.evaluate(() => {
		window.__v2.run = false;
		return window.__v2.maxCells;
	});
	const v2Stats = await page2.evaluate(() => [...window.__ikengaLocalEcho].at(-1).stats());
	console.log(
		`[${mode}] second viewer: overlay cells ever painted ${v2Max}, predicted ${v2Stats.predicted}, ` +
			`tails identical ${v1 === v2}`
	);
	results.scenarios.push({
		label: 'second-viewer',
		viewer1VisibleMedianMs: typed.visibleMedianMs,
		viewer2MaxOverlayCells: v2Max,
		viewer2Predicted: v2Stats.predicted,
		tailsIdentical: v1 === v2,
		viewer1Tail: v1,
		viewer2Tail: v2,
	});
	await page2.close();
	await page.locator('.xterm').first().click();
}

results.statsBeforeClaude = await page.evaluate(() => window.__le.stats());
fs.writeFileSync(path.join(out, 'buffer.txt'), await page.evaluate(() => window.__le.dumpBuffer()));
await page.screenshot({ path: path.join(out, 'bash-final.png') });

// 6. Claude Code's input box (alternate screen).
if (withClaude) {
	await page.keyboard.type('claude', { delay: 20 });
	await enter();
	await page.waitForFunction(
		() => {
			const t = window.__le.term();
			if (t.buffer.active.type !== 'alternate') return false;
			const { y } = window.__le.cursorRow();
			return window.__le.rowText(y).includes('❯');
		},
		null,
		{ timeout: 30_000 }
	);
	await waitQuiet(3000);
	results.scenarios.push(
		await measureTyping('claude', 'hello from lagos, typing on a phone', { screenshotAt: 20 })
	);
	await page.screenshot({ path: path.join(out, 'claude-final.png') });
	results.claudeInputRow = await page.evaluate(() =>
		window.__le.rowText(window.__le.cursorRow().y).trimEnd()
	);
	// Clear the input (^C), then exit (^C ^C).
	await page.keyboard.press('Control+c');
	await waitQuiet(1000);
	await page.keyboard.press('Control+c');
	await waitQuiet(300);
	await page.keyboard.press('Control+c');
	await waitQuiet(2500);
}

results.statsFinal = await page.evaluate(() => window.__le.stats());
fs.writeFileSync(path.join(out, 'results.json'), JSON.stringify(results, null, 2));
console.log(`[${mode}] stats ${JSON.stringify(results.statsFinal)}`);
await browser.close();
