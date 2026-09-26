// WP-58 — the Editor's Placements section: D-06's ten checkboxes (§1.3),
// bound to the frozen full menu ids rather than D-06's bare `section` /
// `native` labels (§11 item 7 — the file always stores the full id).

import {
	NATIVE_TOP_CHOICES,
	PLACEMENT_IDS,
	PLACEMENT_LABELS,
	type PlacementFlags,
	type PlacementId,
	SECTION_CHOICES,
} from './form-model';

export interface PlacementsFieldsProps {
	placements: PlacementFlags;
	onToggle: (id: PlacementId, checked: boolean) => void;
	filesGlob: string;
	onChangeFilesGlob: (value: string) => void;
	sectionId: string;
	onChangeSectionId: (value: string) => void;
	nativeTop: string;
	onChangeNativeTop: (value: string) => void;
}

export function PlacementsFields({
	placements,
	onToggle,
	filesGlob,
	onChangeFilesGlob,
	sectionId,
	onChangeSectionId,
	nativeTop,
	onChangeNativeTop,
}: PlacementsFieldsProps) {
	return (
		<div className="checks">
			{PLACEMENT_IDS.map((id) => (
				<label key={id} className="ck">
					<input type="checkbox" checked={placements[id]} onChange={(e) => onToggle(id, e.target.checked)} />
					<span>
						<span className="t1">{PLACEMENT_LABELS[id]}</span>
						{id === 'files' && <span className="t2">Shown only for files matching the glob.</span>}
						{id === 'section' && <span className="t2">Pick which Explorer section.</span>}
						{id === 'native' && <span className="t2">Pick which top-level menu (macOS).</span>}
					</span>
					{id === 'files' && (
						<span className="whenfield">
							<input
								type="text"
								aria-label="File glob"
								placeholder="*.{ts,rs}"
								value={filesGlob}
								onClick={(e) => e.preventDefault()}
								onChange={(e) => onChangeFilesGlob(e.target.value)}
							/>
						</span>
					)}
					{id === 'section' && (
						<span className="whenfield">
							<select
								aria-label="Explorer section"
								value={sectionId}
								onClick={(e) => e.preventDefault()}
								onChange={(e) => onChangeSectionId(e.target.value)}
							>
								{SECTION_CHOICES.map((s) => (
									<option key={s.id} value={s.id}>
										{s.label}
									</option>
								))}
							</select>
						</span>
					)}
					{id === 'native' && (
						<span className="whenfield">
							<select
								aria-label="Native top menu"
								value={nativeTop}
								onClick={(e) => e.preventDefault()}
								onChange={(e) => onChangeNativeTop(e.target.value)}
							>
								{NATIVE_TOP_CHOICES.map((n) => (
									<option key={n.id} value={n.id}>
										{n.label}
									</option>
								))}
							</select>
						</span>
					)}
				</label>
			))}
		</div>
	);
}
