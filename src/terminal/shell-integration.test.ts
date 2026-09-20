import { describe, expect, it } from 'vitest';
import { execSync } from 'node:child_process';
import { readFileSync, existsSync } from 'node:fs';
import { resolve } from 'node:path';

describe('Shell Integration Scripts (WP-08 / T-10)', () => {
	const bashScriptPath = resolve(__dirname, 'shell-integration/bash.sh');
	const zshScriptPath = resolve(__dirname, 'shell-integration/zsh.zsh');
	const fishScriptPath = resolve(__dirname, 'shell-integration/fish.fish');

	it('ensures shell integration scripts exist and contain required OSC 133 markers', () => {
		expect(existsSync(bashScriptPath)).toBe(true);
		expect(existsSync(zshScriptPath)).toBe(true);
		expect(existsSync(fishScriptPath)).toBe(true);

		const bash = readFileSync(bashScriptPath, 'utf8');
		expect(bash).toContain('133;A');
		expect(bash).toContain('133;B');
		expect(bash).toContain('133;C');
		expect(bash).toContain('133;D');

		const zsh = readFileSync(zshScriptPath, 'utf8');
		expect(zsh).toContain('133;A');
		expect(zsh).toContain('133;B');
		expect(zsh).toContain('133;C');
		expect(zsh).toContain('133;D');

		const fish = readFileSync(fishScriptPath, 'utf8');
		expect(fish).toContain('133;A');
		expect(fish).toContain('133;B');
		expect(fish).toContain('133;C');
		expect(fish).toContain('133;D');
	});

	it.skipIf(process.platform === 'win32')(
		'runs bash.sh under bash and confirms OSC 133 emission with exit code capture',
		() => {
		const cmd = `PROMPT_COMMAND=". '${bashScriptPath}'" bash -i << 'INPUT' 2>&1
echo "HELLO"
false
exit 0
INPUT`;
		const output = execSync(cmd, { shell: '/bin/bash' }).toString('utf8');

		// Contains OSC 133 sequences
		expect(output).toContain('\x1b]133;B\x07');
		expect(output).toContain('\x1b]133;C\x07');
		expect(output).toContain('\x1b]133;A\x07');
		// Success exit code 0
		expect(output).toContain('\x1b]133;D;0\x07');
		// Failure exit code 1
		expect(output).toContain('\x1b]133;D;1\x07');
	});
});
