// Frontmatter Form Editor (WP-25)
//
// Structured form editor for primitive frontmatter (`SKILL.md`, `agent.md`, `command.md`).
// Parses and serializes YAML frontmatter between `---` fences while strictly preserving
// the markdown body and all `<!-- ikenga:auto -->` fences.

import { useState, useMemo } from 'react';
import { FileText, Sliders, Shield, Code } from 'lucide-react';
import { parseAutoFences } from '@/lib/editor/fence-decorator';

export interface FrontmatterData {
	name?: string;
	description?: string;
	allowedTools?: string[];
	model?: string;
	argumentHint?: string;
	customFields: Record<string, string>;
}

export interface FrontmatterFormEditorProps {
	value: string;
	onChange: (newValue: string) => void;
	readOnly?: boolean;
}

/**
 * Splits document into YAML frontmatter string and body string.
 */
export function splitFrontmatter(text: string): { frontmatterRaw: string; bodyRaw: string; hasFrontmatter: boolean } {
	if (!text.startsWith('---')) {
		return { frontmatterRaw: '', bodyRaw: text, hasFrontmatter: false };
	}

	const endIdx = text.indexOf('\n---', 3);
	if (endIdx === -1) {
		return { frontmatterRaw: '', bodyRaw: text, hasFrontmatter: false };
	}

	const frontmatterRaw = text.slice(3, endIdx).trim();
	const bodyRaw = text.slice(endIdx + 4).replace(/^\r?\n/, '');

	return { frontmatterRaw, bodyRaw, hasFrontmatter: true };
}

/**
 * Parses frontmatter YAML lines into structured object.
 */
export function parseFrontmatter(raw: string): FrontmatterData {
	const data: FrontmatterData = {
		customFields: {},
	};

	const lines = raw.split(/\r?\n/);
	for (const line of lines) {
		const colonIdx = line.indexOf(':');
		if (colonIdx === -1) continue;

		const key = line.slice(0, colonIdx).trim().toLowerCase();
		const val = line.slice(colonIdx + 1).trim();

		switch (key) {
			case 'name':
				data.name = val;
				break;
			case 'description':
				data.description = val;
				break;
			case 'allowed-tools':
			case 'tools':
				data.allowedTools = val
					.split(',')
					.map((t) => t.trim())
					.filter(Boolean);
				break;
			case 'model':
				data.model = val;
				break;
			case 'argument-hint':
				data.argumentHint = val;
				break;
			default:
				if (key) {
					data.customFields[key] = val;
				}
				break;
		}
	}

	return data;
}

/**
 * Serializes structured frontmatter back into clean YAML format.
 */
export function serializeFrontmatter(data: FrontmatterData): string {
	const lines: string[] = [];

	if (data.name) lines.push(`name: ${data.name}`);
	if (data.description) lines.push(`description: ${data.description}`);
	if (data.allowedTools && data.allowedTools.length > 0) {
		lines.push(`allowed-tools: ${data.allowedTools.join(', ')}`);
	}
	if (data.model) lines.push(`model: ${data.model}`);
	if (data.argumentHint) lines.push(`argument-hint: ${data.argumentHint}`);

	for (const [k, v] of Object.entries(data.customFields)) {
		lines.push(`${k}: ${v}`);
	}

	return lines.join('\n');
}

/**
 * Combines serialized frontmatter and body into single document.
 */
export function assembleDocument(frontmatterStr: string, bodyStr: string): string {
	if (!frontmatterStr.trim()) {
		return bodyStr;
	}
	return `---\n${frontmatterStr.trim()}\n---\n\n${bodyStr.replace(/^\r?\n/, '')}`;
}

export function FrontmatterFormEditor({ value, onChange, readOnly = false }: FrontmatterFormEditorProps) {
	const [mode, setMode] = useState<'form' | 'raw'>('form');

	const { frontmatterRaw, bodyRaw } = useMemo(() => splitFrontmatter(value), [value]);
	const frontmatter = useMemo(() => parseFrontmatter(frontmatterRaw), [frontmatterRaw]);
	const autoFences = useMemo(() => parseAutoFences(bodyRaw), [bodyRaw]);

	const updateField = (updater: (prev: FrontmatterData) => FrontmatterData) => {
		if (readOnly) return;
		const next = updater(frontmatter);
		const newFmStr = serializeFrontmatter(next);
		const newDoc = assembleDocument(newFmStr, bodyRaw);
		onChange(newDoc);
	};

	return (
		<div className="flex flex-col h-full bg-[var(--surface-base)] border border-[var(--border-subtle)] rounded-lg overflow-hidden" data-testid="frontmatter-editor">
			{/* Mode Header Bar */}
			<div className="flex items-center justify-between px-4 py-2 border-b border-[var(--border-subtle)] bg-[var(--surface-raised)]">
				<div className="flex items-center gap-2 text-xs">
					<Sliders className="h-3.5 w-3.5 text-[var(--accent-fg)]" />
					<span className="font-semibold text-[var(--fg-base)]">Frontmatter Form</span>
					{autoFences.length > 0 && (
						<span className="ml-2 px-2 py-0.5 rounded-full text-[10px] font-mono bg-[var(--accent-subtle)] text-[var(--accent-fg)] border border-[var(--accent-border)] flex items-center gap-1">
							<Shield className="h-2.5 w-2.5" />
							{autoFences.length} Chi fence{autoFences.length > 1 ? 's' : ''} protected
						</span>
					)}
				</div>

				<div className="flex items-center gap-1 bg-[var(--surface-sunken)] p-0.5 rounded border border-[var(--border-subtle)]">
					<button
						type="button"
						onClick={() => setMode('form')}
						className={`px-2 py-1 rounded text-xs transition-colors flex items-center gap-1 ${
							mode === 'form'
								? 'bg-[var(--surface-raised)] text-[var(--fg-base)] font-medium shadow-sm'
								: 'text-[var(--fg-muted)] hover:text-[var(--fg-base)]'
						}`}
						data-testid="mode-form-btn"
					>
						<Sliders className="h-3 w-3" />
						<span>Form</span>
					</button>
					<button
						type="button"
						onClick={() => setMode('raw')}
						className={`px-2 py-1 rounded text-xs transition-colors flex items-center gap-1 ${
							mode === 'raw'
								? 'bg-[var(--surface-raised)] text-[var(--fg-base)] font-medium shadow-sm'
								: 'text-[var(--fg-muted)] hover:text-[var(--fg-base)]'
						}`}
						data-testid="mode-raw-btn"
					>
						<Code className="h-3 w-3" />
						<span>Raw</span>
					</button>
				</div>
			</div>

			{/* Editor Content Area */}
			{mode === 'form' ? (
				<div className="p-4 space-y-4 overflow-y-auto flex-1 text-xs">
					{/* Name field */}
					<div className="space-y-1">
						<label className="font-medium text-[var(--fg-base)]">Name</label>
						<input
							type="text"
							disabled={readOnly}
							value={frontmatter.name || ''}
							onChange={(e) => updateField((prev) => ({ ...prev, name: e.target.value }))}
							placeholder="e.g. release-notes"
							className="w-full px-2.5 py-1.5 rounded border border-[var(--border-subtle)] bg-[var(--surface-input)] text-[var(--fg-base)] font-mono text-xs focus:outline-none focus:border-[var(--border-focus)]"
							data-testid="field-name"
						/>
					</div>

					{/* Description field */}
					<div className="space-y-1">
						<label className="font-medium text-[var(--fg-base)]">Description</label>
						<textarea
							rows={2}
							disabled={readOnly}
							value={frontmatter.description || ''}
							onChange={(e) => updateField((prev) => ({ ...prev, description: e.target.value }))}
							placeholder="Human and routing description"
							className="w-full px-2.5 py-1.5 rounded border border-[var(--border-subtle)] bg-[var(--surface-input)] text-[var(--fg-base)] text-xs focus:outline-none focus:border-[var(--border-focus)] resize-none"
							data-testid="field-description"
						/>
					</div>

					{/* Allowed tools / chips */}
					<div className="space-y-1">
						<label className="font-medium text-[var(--fg-base)]">Allowed Tools</label>
						<input
							type="text"
							disabled={readOnly}
							value={(frontmatter.allowedTools || []).join(', ')}
							onChange={(e) =>
								updateField((prev) => ({
									...prev,
									allowedTools: e.target.value
										.split(',')
										.map((t) => t.trim())
										.filter(Boolean),
								}))
							}
							placeholder="e.g. Read, Grep, Glob, Bash"
							className="w-full px-2.5 py-1.5 rounded border border-[var(--border-subtle)] bg-[var(--surface-input)] text-[var(--fg-base)] font-mono text-xs focus:outline-none focus:border-[var(--border-focus)]"
							data-testid="field-tools"
						/>
					</div>

					{/* Model override */}
					<div className="space-y-1">
						<label className="font-medium text-[var(--fg-base)]">Model Tier (Optional)</label>
						<input
							type="text"
							disabled={readOnly}
							value={frontmatter.model || ''}
							onChange={(e) => updateField((prev) => ({ ...prev, model: e.target.value }))}
							placeholder="e.g. claude-3-5-sonnet"
							className="w-full px-2.5 py-1.5 rounded border border-[var(--border-subtle)] bg-[var(--surface-input)] text-[var(--fg-base)] font-mono text-xs focus:outline-none focus:border-[var(--border-focus)]"
							data-testid="field-model"
						/>
					</div>

					{/* Read-only body preview note */}
					<div className="pt-3 border-t border-[var(--border-subtle)] text-[11px] text-[var(--fg-muted)] flex items-center gap-1.5">
						<FileText className="h-3 w-3 flex-shrink-0" />
						<span>Markdown body ({bodyRaw.length} chars) preserved untouched.</span>
					</div>
				</div>
			) : (
				<textarea
					disabled={readOnly}
					value={value}
					onChange={(e) => onChange(e.target.value)}
					className="flex-1 p-4 font-mono text-xs bg-[var(--surface-input)] text-[var(--fg-base)] resize-none focus:outline-none"
					data-testid="raw-textarea"
				/>
			)}
		</div>
	);
}
