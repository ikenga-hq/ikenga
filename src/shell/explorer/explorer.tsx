import { useState, useRef, useCallback, type KeyboardEvent } from 'react';
import { useShellStore } from '@/lib/shell/shell-store';
import { ExplorerHeader } from './explorer-header';
import { builtInSections } from './section-registry';
import { SectionFrame } from './section-frame';

export function Explorer() {
	const activeProjectId = useShellStore((s) => s.activeProjectId);
	const explorerSections = useShellStore((s) => s.explorerSections);
	const setExplorerSectionCollapsed = useShellStore((s) => s.setExplorerSectionCollapsed);
	const moveExplorerSection = useShellStore((s) => s.moveExplorerSection);

	const [hiddenSectionIds, setHiddenSectionIds] = useState<Set<string>>(new Set());
	const previousOpenState = useRef<string[] | null>(null);
	const containerRef = useRef<HTMLDivElement>(null);
	const typeaheadBuffer = useRef<string>('');
	const typeaheadTimer = useRef<number | null>(null);

	const visibleSections = explorerSections.filter((sec) => !hiddenSectionIds.has(sec.id));

	const handleSectionToggle = (sectionId: string, exclusive: boolean) => {
		const currentlyOpen = visibleSections.filter((s) => !s.collapsed).map((s) => s.id);

		if (exclusive) {
			const isAlreadyExclusive = currentlyOpen.length === 1 && currentlyOpen[0] === sectionId;
			if (isAlreadyExclusive && previousOpenState.current && previousOpenState.current.length > 0) {
				// Restore previous open set
				const toRestore = previousOpenState.current;
				previousOpenState.current = null;
				for (const s of explorerSections) {
					const shouldBeOpen = toRestore.includes(s.id);
					if (s.collapsed === shouldBeOpen) {
						setExplorerSectionCollapsed(s.id, !shouldBeOpen);
					}
				}
			} else {
				// Save current open state and make this section exclusive
				previousOpenState.current = currentlyOpen;
				for (const s of explorerSections) {
					if (s.id === sectionId) {
						setExplorerSectionCollapsed(s.id, false);
					} else {
						if (!s.collapsed) setExplorerSectionCollapsed(s.id, true);
					}
				}
			}
		} else {
			const target = explorerSections.find((s) => s.id === sectionId);
			if (target) {
				setExplorerSectionCollapsed(sectionId, !target.collapsed);
			}
		}
	};

	const handleCollapseOthers = (sectionId: string) => {
		for (const s of explorerSections) {
			if (s.id === sectionId) {
				if (s.collapsed) setExplorerSectionCollapsed(s.id, false);
			} else {
				if (!s.collapsed) setExplorerSectionCollapsed(s.id, true);
			}
		}
	};

	const handleHideSection = (sectionId: string) => {
		setHiddenSectionIds((prev) => new Set([...prev, sectionId]));
	};

	const getVisibleRows = useCallback((): HTMLElement[] => {
		if (!containerRef.current) return [];
		const rows = Array.from(
			containerRef.current.querySelectorAll<HTMLElement>('[data-explorer-row]')
		);
		return rows.filter((el) => el.offsetParent !== null);
	}, []);

	const getSectionHeaders = useCallback((): HTMLElement[] => {
		if (!containerRef.current) return [];
		const headers = Array.from(
			containerRef.current.querySelectorAll<HTMLElement>('[data-explorer-header="true"]')
		);
		return headers.filter((el) => el.offsetParent !== null);
	}, []);

	const handleKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
		// ⌘⇧[ / ⌘⇧] → previous / next section header (spec §6.3)
		if ((e.key === '[' || e.key === ']') && (e.metaKey || e.ctrlKey) && e.shiftKey) {
			e.preventDefault();
			const headers = getSectionHeaders();
			if (headers.length === 0) return;
			const currentFocused = document.activeElement as HTMLElement | null;
			const currentIndex = headers.findIndex((h) => h === currentFocused || h.contains(currentFocused));
			if (e.key === '[') {
				const prevIndex = currentIndex > 0 ? currentIndex - 1 : headers.length - 1;
				headers[prevIndex]?.focus();
			} else {
				const nextIndex = currentIndex < headers.length - 1 ? currentIndex + 1 : 0;
				headers[nextIndex]?.focus();
			}
			return;
		}

		const visibleRows = getVisibleRows();
		if (visibleRows.length === 0) return;

		const currentFocused = document.activeElement as HTMLElement | null;
		let currentIndex = visibleRows.findIndex((r) => r === currentFocused || r.contains(currentFocused));

		if (currentIndex === -1 && (e.key === 'ArrowDown' || e.key === 'ArrowUp' || e.key === 'Home' || e.key === 'End')) {
			visibleRows[0]?.focus();
			return;
		}

		if (e.key === 'ArrowDown') {
			e.preventDefault();
			const next = Math.min(currentIndex + 1, visibleRows.length - 1);
			visibleRows[next]?.focus();
		} else if (e.key === 'ArrowUp') {
			e.preventDefault();
			const prev = Math.max(currentIndex - 1, 0);
			visibleRows[prev]?.focus();
		} else if (e.key === 'Home') {
			e.preventDefault();
			visibleRows[0]?.focus();
		} else if (e.key === 'End') {
			e.preventDefault();
			visibleRows[visibleRows.length - 1]?.focus();
		} else if (e.key === 'ArrowRight') {
			// If on a collapsed section header, expand it
			const row = visibleRows[currentIndex];
			if (row?.getAttribute('data-explorer-header') === 'true') {
				const isExpanded = row.getAttribute('aria-expanded') === 'true';
				if (!isExpanded) {
					e.preventDefault();
					row.click();
				}
			}
		} else if (e.key === 'ArrowLeft') {
			const row = visibleRows[currentIndex];
			if (row?.getAttribute('data-explorer-header') === 'true') {
				const isExpanded = row.getAttribute('aria-expanded') === 'true';
				if (isExpanded) {
					e.preventDefault();
					row.click();
				}
			} else if (row) {
				// Move focus to parent section header
				const sectionEl = row.closest('[data-explorer-section]');
				const header = sectionEl?.querySelector<HTMLElement>('[data-explorer-header="true"]');
				if (header) {
					e.preventDefault();
					header.focus();
				}
			}
		} else if (e.key.length === 1 && !e.metaKey && !e.ctrlKey && !e.altKey) {
			// Typeahead buffer (500ms)
			typeaheadBuffer.current += e.key.toLowerCase();
			if (typeaheadTimer.current !== null) {
				window.clearTimeout(typeaheadTimer.current);
			}
			typeaheadTimer.current = window.setTimeout(() => {
				typeaheadBuffer.current = '';
			}, 500);

			const query = typeaheadBuffer.current;
			// Find next row starting with query (cycle from current index + 1)
			const count = visibleRows.length;
			for (let i = 1; i <= count; i++) {
				const checkIndex = (currentIndex + i) % count;
				const text = visibleRows[checkIndex]?.textContent?.trim().toLowerCase() ?? '';
				if (text.startsWith(query)) {
					visibleRows[checkIndex]?.focus();
					break;
				}
			}
		}
	};

	const context = { projectId: activeProjectId };

	return (
		<div className="flex flex-col h-full bg-background text-foreground">
			<ExplorerHeader />
			<div
				className="flex-1 overflow-y-auto focus:outline-none"
				ref={containerRef}
				tabIndex={0}
				onKeyDown={handleKeyDown}
				role="region"
				aria-label="Explorer sections"
			>
				{visibleSections.map((sec, idx) => {
					const def = builtInSections.find((d) => d.id === sec.id);
					if (!def) return null;
					return (
						<SectionFrame
							key={sec.id}
							section={def}
							context={context}
							isOpen={!sec.collapsed}
							onToggle={(exclusive) => handleSectionToggle(sec.id, exclusive)}
							onCollapseOthers={() => handleCollapseOthers(sec.id)}
							onHideSection={() => handleHideSection(sec.id)}
							canMoveUp={idx > 0}
							canMoveDown={idx < visibleSections.length - 1}
							onMoveUp={() => moveExplorerSection(sec.id, -1)}
							onMoveDown={() => moveExplorerSection(sec.id, 1)}
						>
							{def.render(context)}
						</SectionFrame>
					);
				})}
			</div>
		</div>
	);
}
