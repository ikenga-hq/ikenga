// Store install-sheet derivations (WP-15 / locked D-02).
//
// The registry index carries only name / latest / kind / description, so a
// row's closure and permission asks can only be read from the per-pkg detail
// file (`pkgs/<short>.json` → `versions[].manifest`). Everything here is a
// pure function of that manifest: when the detail hasn't been fetched the
// callers render the design's "… not read" labels instead of guessing.

import type { StorePkgVersion } from '@/lib/registry/client';
import type { NgwaKind } from '@ikenga/contract';

export type StoreManifest = StorePkgVersion['manifest'];

function plural(n: number, one: string, many = `${one}s`): string {
	return `${n} ${n === 1 ? one : many}`;
}

const REQUIRE_KIND_LABEL: Record<string, [string, string]> = {
	bundle: ['bundle', 'bundles'],
	skill: ['skill', 'skills'],
	agent: ['agent', 'agents'],
	command: ['command', 'commands'],
	hook: ['hook', 'hooks'],
	mcp: ['MCP server', 'MCP servers'],
};

/** Kinds that declare intent and never grant themselves anything. */
function isDeclarative(kind: NgwaKind): boolean {
	return kind === 'skill' || kind === 'bundle';
}

/**
 * The row's closure chip: what installing this pulls in alongside it.
 * `null` manifest = the detail file hasn't been read yet.
 */
export function closureLabel(kind: NgwaKind, manifest: StoreManifest | null): string {
	if (!manifest) return kind === 'bundle' ? 'bundle, published as one unit' : 'closure not read';
	const requires = manifest.requires ?? [];
	if (kind === 'bundle') {
		const skills = requires.filter((r) => r.kind === 'skill').length;
		return skills > 0
			? `bundle · ${plural(skills, 'skill')}, published as one unit`
			: 'bundle, published as one unit';
	}

	// Ordered counts: required primitives first, then the pkg's own sidecars
	// and MCP servers (which install with it).
	const counts = new Map<string, number>();
	const bump = (key: string, n: number) => {
		if (n > 0) counts.set(key, (counts.get(key) ?? 0) + n);
	};
	for (const r of requires) bump(r.kind, 1);
	bump('sidecar', manifest.sidecars?.length ?? 0);
	bump('mcp', manifest.mcp?.length ?? 0);

	const parts = [...counts.entries()].map(([k, n]) => {
		const [one, many] = REQUIRE_KIND_LABEL[k] ?? [k, `${k}s`];
		return plural(n, one, many);
	});
	if (requires.length > 0) return `also installs ${parts.join(' · ')}`;
	if (parts.length > 0) return parts.join(' · ');
	return 'no requires';
}

type PermKey =
	| 'shell.execute'
	| 'fs.read'
	| 'fs.write'
	| 'net'
	| 'sqlite.tables'
	| 'supabase.tables'
	| 'vault.keys'
	| 'engine'
	| 'notify'
	| 'events';

function perm(manifest: StoreManifest, key: PermKey): string[] {
	const p = (manifest.permissions ?? {}) as Partial<Record<PermKey, string[]>>;
	return (p[key] ?? []).filter((s) => typeof s === 'string' && s.length > 0);
}

/** `$pkg_data/**` → `$pkg_data`; leaves non-glob paths alone. */
function pathRoot(p: string): string {
	return p.replace(/\/\*\*?$/, '');
}

/** `https://esm.sh` → `esm.sh`; `http://127.0.0.1:*` → `127.0.0.1:*`. */
function hostOf(u: string): string {
	return u.replace(/^[a-z]+:\/\//i, '').replace(/\/$/, '');
}

/**
 * The row's asks chip, e.g. `asks: shell.execute · fs.write $pkg_data ·
 * net (5 hosts) · vault.keys (3)`. `null` manifest → `permissions not read`.
 */
export function asksLabel(kind: NgwaKind, manifest: StoreManifest | null): string {
	if (!manifest) return 'permissions not read';
	const parts: string[] = [];
	if (perm(manifest, 'shell.execute').length) parts.push('shell.execute');
	const fsw = perm(manifest, 'fs.write');
	const fsr = perm(manifest, 'fs.read');
	if (fsw.length)
		parts.push(`fs.write ${pathRoot(fsw[0])}${fsw.length > 1 ? ` (+${fsw.length - 1})` : ''}`);
	else if (fsr.length)
		parts.push(`fs.read ${pathRoot(fsr[0])}${fsr.length > 1 ? ` (+${fsr.length - 1})` : ''}`);
	const net = perm(manifest, 'net');
	if (net.length) parts.push(`net (${plural(net.length, 'host')})`);
	const vault = perm(manifest, 'vault.keys');
	if (vault.length) parts.push(`vault.keys (${vault.length})`);
	const tables = [...perm(manifest, 'sqlite.tables'), ...perm(manifest, 'supabase.tables')];
	if (tables.length) parts.push(`tables (${tables.length})`);
	const engine = perm(manifest, 'engine');
	if (engine.length) parts.push(`engine.${engine.join(',')}`);
	if (perm(manifest, 'notify').length) parts.push('notify');
	const events = perm(manifest, 'events');
	if (events.length) parts.push(`events (${events.length})`);

	if (parts.length === 0) {
		return isDeclarative(kind) ? 'asks: declares intent, never grants' : 'asks: nothing';
	}
	return `asks: ${parts.join(' · ')}`;
}

export interface ConsentGroup {
	/** Stable key for the checkbox. */
	id: string;
	/** Permission name(s) as the manifest spells them. */
	label: string;
	/** What granting it lets the pkg do. */
	detail: string;
}

function pathsDetail(paths: string[]): string {
	const joined = paths.join(' · ');
	return paths.every((p) => p.startsWith('$pkg_data'))
		? `${joined} only — inside its own folder.`
		: joined;
}

/**
 * One consent per permission group the manifest declares. Install stays
 * disabled until every group is ticked; an empty list means there is nothing
 * to consent to.
 */
export function consentGroups(manifest: StoreManifest): ConsentGroup[] {
	const out: ConsentGroup[] = [];
	const bins = perm(manifest, 'shell.execute');
	if (bins.length) {
		out.push({
			id: 'shell.execute',
			label: 'shell.execute',
			detail: `${bins.join(' · ')} — runs these binaries on your machine.`,
		});
	}
	const net = perm(manifest, 'net');
	if (net.length) {
		out.push({ id: 'net', label: 'net', detail: net.map(hostOf).join(' · ') });
	}
	const fsr = perm(manifest, 'fs.read');
	const fsw = perm(manifest, 'fs.write');
	const sameFs = fsr.length > 0 && fsr.length === fsw.length && fsr.every((p) => fsw.includes(p));
	if (sameFs) {
		out.push({ id: 'fs', label: 'fs.read · fs.write', detail: pathsDetail(fsr) });
	} else {
		if (fsr.length) out.push({ id: 'fs.read', label: 'fs.read', detail: pathsDetail(fsr) });
		if (fsw.length) out.push({ id: 'fs.write', label: 'fs.write', detail: pathsDetail(fsw) });
	}
	const vault = perm(manifest, 'vault.keys');
	if (vault.length) {
		out.push({
			id: 'vault.keys',
			label: 'vault.keys',
			detail: `${vault.join(' · ')} — read from Stronghold, never shown to the iframe.`,
		});
	}
	const sqlite = perm(manifest, 'sqlite.tables');
	if (sqlite.length) {
		out.push({ id: 'sqlite.tables', label: 'sqlite.tables', detail: sqlite.join(' · ') });
	}
	const supabase = perm(manifest, 'supabase.tables');
	if (supabase.length) {
		out.push({ id: 'supabase.tables', label: 'supabase.tables', detail: supabase.join(' · ') });
	}
	const engine = perm(manifest, 'engine');
	if (engine.length) {
		out.push({
			id: 'engine',
			label: `engine.${engine.join(' · ')}`,
			detail: 'can send work to your active engine session.',
		});
	}
	const notify = perm(manifest, 'notify');
	if (notify.length) {
		out.push({
			id: 'notify',
			label: 'notify',
			detail: 'raises OS notifications, which reach you even when Ikenga is not focused.',
		});
	}
	const events = perm(manifest, 'events');
	if (events.length) {
		out.push({ id: 'events', label: 'events', detail: events.join(' · ') });
	}
	return out;
}

/** Tarball size for the sheet footer. */
export function formatBytes(n: number | undefined): string | null {
	if (n === undefined || !Number.isFinite(n)) return null;
	if (n < 1024) return `${n} B`;
	if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
	const mb = n / (1024 * 1024);
	return `${mb < 10 ? mb.toFixed(1) : Math.round(mb)} MB`;
}
