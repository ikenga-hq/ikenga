// WP-58 — the Editor's Key section: the shared `KeyRecorder`, an optional
// `when` override, and the DEC-59 inline conflict card (G-ACTIONS §5) with
// its three resolutions (Unbind the other / Restrict to `<when>` / Choose
// another key).

import { AlertTriangle, Check } from 'lucide-react';
import { KeyRecorder } from '../shared/key-recorder';
import { Kbd } from '../shared/kbd';
import type { EditorConflict } from './form-model';

export interface KeyFieldsProps {
	keyCombo: string;
	onChangeKey: (combo: string) => void;
	whenTouched: boolean;
	whenValue: string;
	derivedWhen: string;
	onChangeWhen: (value: string) => void;
	whenInvalid: boolean;
	conflict: EditorConflict | null;
	restrictionSuggestion: string;
	unbindPending: boolean;
	unbindError: string | null;
	onUnbindOther: () => void;
	onRestrict: () => void;
	onChooseAnother: () => void;
}

export function KeyFields({
	keyCombo,
	onChangeKey,
	whenTouched,
	whenValue,
	derivedWhen,
	onChangeWhen,
	whenInvalid,
	conflict,
	restrictionSuggestion,
	unbindPending,
	unbindError,
	onUnbindOther,
	onRestrict,
	onChooseAnother,
}: KeyFieldsProps) {
	return (
		<div className="q">
			<span className="hint">Click, then press the combination. Press a second within a moment for a chord.</span>
			<div style={{ display: 'flex', gap: 'var(--space-2)', alignItems: 'center', marginTop: 'var(--space-2)' }}>
				<KeyRecorder value={keyCombo || null} onRecord={onChangeKey} aria-label="Record a keyboard shortcut" />
				<button type="button" className="btn" onClick={() => onChangeKey('')}>
					Clear
				</button>
			</div>

			<div className="q" style={{ marginTop: 'var(--space-3)' }}>
				<label className="lab" htmlFor="edWhen">
					When
				</label>
				<span className="hint">
					Restricts when the key fires. Left blank, it follows your Files placement's glob:{' '}
					<span className="mono">{derivedWhen}</span>.
				</span>
				<div className={`field${whenInvalid ? ' invalid' : ''}`}>
					<input
						id="edWhen"
						type="text"
						value={whenTouched ? whenValue : derivedWhen}
						placeholder={derivedWhen}
						onChange={(e) => onChangeWhen(e.target.value)}
					/>
				</div>
				{whenInvalid && (
					<span className="err" role="alert">
						Not a valid `when` expression (§4) — the key still records, but this won't save until it parses.
					</span>
				)}
			</div>

			{conflict ? (
				<div className="conflictbox" role="alert">
					<div className="hd">
						<AlertTriangle className="h-3 w-3" /> Conflict
					</div>
					<div style={{ marginTop: 4 }}>
						Used by: <b>{conflict.other.command}</b> ({conflict.other.when || 'always'})
					</div>
					{unbindError && (
						<div style={{ marginTop: 4, color: 'var(--danger)' }} role="alert">
							{unbindError}
						</div>
					)}
					<div className="acts">
						<button type="button" className="btn" disabled={unbindPending} onClick={onUnbindOther}>
							Unbind the other
						</button>
						<button type="button" className="btn" onClick={onRestrict}>
							Restrict to <span className="mono">{restrictionSuggestion}</span>
						</button>
						<button type="button" className="btn" onClick={onChooseAnother}>
							Choose another key
						</button>
					</div>
				</div>
			) : keyCombo ? (
				<div className="okbox">
					<Check className="h-3 w-3" /> Free — nothing else claims <Kbd combo={keyCombo} />.
				</div>
			) : null}
		</div>
	);
}
