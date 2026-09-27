import type { SettingsSectionId } from '@/shell/settings/nav';
import { SETTINGS_SECTIONS } from '@/shell/settings/nav';

export interface SettingsSearchHit {
	sectionId: SettingsSectionId;
	sectionLabel: string;
	field: string | null;
	label: string;
	hayLabel: string;
	group: string;
}

function searchableHits(): SettingsSearchHit[] {
	const hits: SettingsSearchHit[] = [];
	for (const section of SETTINGS_SECTIONS) {
		for (const meta of section.fields) {
			hits.push({
				sectionId: section.id,
				sectionLabel: section.label,
				field: meta.field,
				label: meta.label,
				hayLabel: meta.label,
				group: [meta.help ?? '', meta.keywords ?? ''].join(' '),
			});
		}
	}
	return hits;
}

const HITS = searchableHits();

export function searchSettings(query: string): SettingsSearchHit[] {
	const q = query.trim().toLowerCase();
	if (!q) return [];
	return HITS.filter((hit) =>
		`${hit.label} ${hit.group} ${hit.sectionLabel}`.toLowerCase().includes(q)
	);
}

function Marked({ text, query }: { text: string; query: string }) {
	const q = query.trim().toLowerCase();
	const index = text.toLowerCase().indexOf(q);
	if (!q || index < 0) return <>{text}</>;
	return (
		<>
			{text.slice(0, index)}
			<mark className="rounded-sm bg-primary/20 text-foreground">
				{text.slice(index, index + q.length)}
			</mark>
			{text.slice(index + q.length)}
		</>
	);
}

interface SettingsSearchResultsProps {
	query: string;
	onGo: (sectionId: SettingsSectionId) => void;
}

export function SettingsSearchResults({ query, onGo }: SettingsSearchResultsProps) {
	const hits = searchSettings(query);
	const sections = new Set(hits.map((hit) => hit.sectionId)).size;
	return (
		<div className="mx-auto w-full max-w-[720px] px-6 py-6">
			<div className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
				{hits.length} field{hits.length === 1 ? '' : 's'} matching “{query.trim()}” · {sections}{' '}
				section{sections === 1 ? '' : 's'}
			</div>
			{hits.length === 0 ? (
				<div className="mt-4 text-sm text-muted-foreground">
					Nothing matches that. Search runs over every field label, its help text and its options,
					in all nine sections.
				</div>
			) : (
				<ul className="mt-3 divide-y divide-border overflow-hidden rounded-lg border border-border-soft bg-card">
					{hits.map((hit) => (
						<li key={`${hit.sectionId}:${hit.field ?? hit.label}`}>
							<button
								type="button"
								onClick={() => onGo(hit.sectionId)}
								className="flex w-full items-center gap-3 px-4 py-2.5 text-left outline-none transition-colors focus-visible:bg-accent hover:bg-accent/50"
							>
								<span className="w-32 shrink-0 truncate font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
									{hit.sectionLabel}
								</span>
								<span className="min-w-0 flex-1">
									<span className="block truncate text-sm text-foreground">
										<Marked text={hit.label} query={query} />
									</span>
									{hit.group.trim() && (
										<span className="block truncate text-xs text-muted-foreground">
											<Marked text={hit.group.trim()} query={query} />
										</span>
									)}
								</span>
								<span className="shrink-0 font-mono text-[10px] text-muted-foreground">
									{hit.sectionId}
								</span>
							</button>
						</li>
					))}
				</ul>
			)}
		</div>
	);
}
