// R57 · Q3 — the catalog generator's content hash MUST equal the Rust
// installer's (`src-tauri/src/server/shared/claude_store/source.rs`
// `content_hash_dir_golden`), or every pinned catalog entry fails to verify at
// install. Same tree, same constants, both sides.
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { afterEach, describe, expect, it } from 'vitest';

import {
	contentHashBytes,
	contentHashDir,
	liftRequires,
	locatePrimitive,
	sortKeysDeep,
} from '../../../scripts/catalog-pin/core';

const dirs: string[] = [];
function tree(files: Record<string, string>): string {
	const d = mkdtempSync(join(tmpdir(), 'r57-pin-'));
	dirs.push(d);
	for (const [rel, body] of Object.entries(files)) {
		const p = join(d, rel);
		mkdirSync(join(p, '..'), { recursive: true });
		writeFileSync(p, body);
	}
	return d;
}
afterEach(() => {
	for (const d of dirs.splice(0)) rmSync(d, { recursive: true, force: true });
});

describe('catalog pin core (mirrors the Rust installer)', () => {
	it('matches the Rust golden content hashes', () => {
		const d = tree({
			'SKILL.md': '---\nname: demo\n---\nbody\n',
			'refs/a.txt': 'alpha',
			'.git/HEAD': 'ignored',
		});
		expect(contentHashDir(d)).toBe(
			'sha256-a455daeda4518fad52af890f26b404ecaa0661410226ac0717cd15ee69ba7df6'
		);
		expect(contentHashBytes(Buffer.from('{"event":"PreToolUse","block":[]}'))).toBe(
			'sha256-c784302747a253d95f356d0a4085272693696795e9122c79fefcda93ff1a2195'
		);
	});

	it('locates like the installer and lifts requires', () => {
		const d = tree({
			'skills/groundwork/SKILL.md': 'x',
			'manifest.json': '{"requires":[{"kind":"skill","name":"core"}]}',
			'hooks/floor.json': '{"event":"PreToolUse","block":[]}',
			'.mcp.json': '{"mcpServers":{"r":{"command":"node","args":["a"]}}}',
		});
		const skill = locatePrimitive(d, 'skill', 'groundwork');
		expect(skill).toEqual({ form: 'dir', path: join(d, 'skills', 'groundwork') });
		expect(skill && liftRequires(d, skill)).toEqual([{ kind: 'skill', name: 'core' }]);
		expect(locatePrimitive(d, 'hook', 'floor')?.form).toBe('fragment');
		const mcp = locatePrimitive(d, 'mcp', 'r');
		expect(mcp?.form === 'fragment' && Buffer.from(mcp.bytes).toString()).toBe(
			'{\n  "args": [\n    "a"\n  ],\n  "command": "node"\n}'
		);
		expect(locatePrimitive(d, 'agent', 'nope')).toBeNull();
	});

	it('sorts keys deeply like serde_json', () => {
		expect(JSON.stringify(sortKeysDeep({ b: 1, a: { d: [{ z: 1, y: 2 }], c: 0 } }))).toBe(
			'{"a":{"c":0,"d":[{"y":2,"z":1}]},"b":1}'
		);
	});
});
