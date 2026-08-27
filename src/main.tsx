import { mark } from '@/lib/boot-timing';
mark('boot:js-start');

import './styles.css';
import { installInstrumentation } from '@/lib/iyke/bridge';
import { isDetachedWindow } from '@/lib/window/window-context';
import { installZoom } from '@/lib/window/zoom';

// Install console & error instrumentation immediately so logs are captured from frame 0
installInstrumentation();

// App zoom (⌘/⌃ +/-/0). Installed before either boot path so the persisted
// level is applied while the first paint is still going up — restoring it
// after React mounts makes every launch visibly re-flow.
installZoom();

// Single SPA entry, two boot paths (plans/multi-window WP-05, G-02 — same
// entry + a lazy code-split, NOT a second Vite entry, which would hit Tauri
// #4372). `WindowRegistry::spawn` (Rust) appends `?window=<label>&surfaces=…`
// to the same app URL as `main`, so a detached window is detected here and
// boots the thin surface-host instead of the full workspace. Each path is a
// dynamic import so the detached window never parses the primary bundle (the
// router tree, every route, the workspace chrome).
// A bare `void import(...).then(...)` (its prior form) had no `.catch()`: one
// transient module-fetch failure rejected as an unhandled rejection, the boot
// function never ran, React never mounted — a permanently blank window with no
// error, no retry, no diagnostics (F-7). Wrap the load so a blip is retried
// once and any hard failure surfaces an actionable screen instead of a blank one.
function loadBoot(): Promise<void> {
	return isDetachedWindow()
		? import('@/boot/detached').then((m) => m.bootDetached())
		: import('@/boot/primary').then((m) => m.bootPrimary());
}

function renderBootError(err: unknown): void {
	const root = document.getElementById('root');
	if (!root) return;
	const msg = err instanceof Error ? err.message : String(err);
	root.replaceChildren();
	const wrap = document.createElement('div');
	wrap.style.cssText =
		'display:flex;flex-direction:column;gap:10px;align-items:center;justify-content:center;height:100vh;font-family:system-ui,sans-serif;color:#e6ded3;background:#1a1611;padding:24px;text-align:center';
	const h = document.createElement('div');
	h.textContent = 'Ikenga failed to start';
	h.style.cssText = 'font-size:15px;font-weight:600';
	const p = document.createElement('div');
	p.textContent = 'A module failed to load — this is usually transient.';
	p.style.cssText = 'font-size:13px;opacity:.7';
	const detail = document.createElement('div');
	detail.textContent = msg.slice(0, 300);
	detail.style.cssText =
		'font-size:11px;opacity:.5;font-family:ui-monospace,monospace;max-width:520px;word-break:break-word';
	const btn = document.createElement('button');
	btn.textContent = 'Reload';
	btn.style.cssText =
		'margin-top:8px;padding:6px 16px;font-size:13px;border-radius:6px;border:1px solid #57503f;background:#2a251d;color:#efe7db;cursor:pointer';
	btn.onclick = () => location.reload();
	wrap.append(h, p, detail, btn);
	root.appendChild(wrap);
}

window.addEventListener('error', (event) => {
	console.error('[main] Uncaught window error:', event.error || event.message);
	const root = document.getElementById('root');
	if (root && root.children.length === 0) {
		renderBootError(event.error || event.message);
	}
});

window.addEventListener('unhandledrejection', (event) => {
	console.error('[main] Unhandled promise rejection:', event.reason);
	const root = document.getElementById('root');
	if (root && root.children.length === 0) {
		renderBootError(event.reason);
	}
});

async function boot(): Promise<void> {
	try {
		await loadBoot();
	} catch (err) {
		console.error('[boot] first attempt failed, retrying once', err);
		try {
			await loadBoot();
		} catch (err2) {
			console.error('[boot] retry failed — surfacing error screen', err2);
			renderBootError(err2);
		}
	}
}

void boot();
