import React from 'react';
import type { LucideIcon } from 'lucide-react';
import {
	Files,
	Shapes,
	TerminalSquare,
	Package,
	Clock,
	CheckSquare,
	FileEdit,
	LayoutGrid,
} from 'lucide-react';

import { FilesSection } from './sections/files';
import { ArtifactsSection } from './sections/artifacts';
import { SessionsSeatsLink, SessionsSection } from './sections/sessions';
import { NgwaProjectSection, useNgwaProjectRowCount } from './sections/ngwa-project';
import { AutomationsSection } from './sections/automations';
import { TodosSection } from './sections/todos';
import { ScratchpadsSection } from './sections/scratchpads';
import { ViewsSection } from './sections/views';

import { useTerminalStore } from '@/terminal/session-store';
import { useGitStatus } from '@/lib/shell/use-git-status';
import { usePkgActivityBarEntries } from '@/lib/pkg/use-activity-bar-entries';

export interface ExplorerSectionContext {
	projectId: string;
}

export interface ExplorerSectionDefinition {
	id: string;
	title: string;
	icon: LucideIcon;
	defaultOrder: number;
	render: (ctx: ExplorerSectionContext) => React.ReactNode;
	badge?: (ctx: ExplorerSectionContext) => string | undefined;
	count?: (ctx: ExplorerSectionContext) => number | undefined;
	useCount?: (ctx: ExplorerSectionContext) => number | undefined;
	/**
	 * WP-71a: controls for the section's header row, at its right end (D-09
	 * LISTING draws the Sessions "Seats" link there). `section-frame.tsx`
	 * renders them beside the header button, never inside it, so clicking one
	 * doesn't collapse the section. They show whether the section is open or
	 * collapsed.
	 */
	headerActions?: (ctx: ExplorerSectionContext) => React.ReactNode;
}

export const builtInSections: ExplorerSectionDefinition[] = [
	{
		id: 'files',
		title: 'Files',
		icon: Files,
		defaultOrder: 0,
		render: (ctx) => React.createElement(FilesSection, ctx),
		useCount: () => {
			const git = useGitStatus();
			return git.data?.files.size ?? 0;
		},
	},
	{
		id: 'artifacts',
		title: 'Artifacts',
		icon: Shapes,
		defaultOrder: 1,
		render: (ctx) => React.createElement(ArtifactsSection, ctx),
	},
	{
		id: 'sessions',
		title: 'Sessions',
		icon: TerminalSquare,
		defaultOrder: 2,
		render: (ctx) => React.createElement(SessionsSection, ctx),
		useCount: () => useTerminalStore((s) => s.tabs.length),
		headerActions: (ctx) => React.createElement(SessionsSeatsLink, ctx),
	},
	{
		id: 'ngwa-project',
		title: 'Ngwa · project',
		icon: Package,
		defaultOrder: 3,
		render: (ctx) => React.createElement(NgwaProjectSection, ctx),
		useCount: (ctx) => useNgwaProjectRowCount(ctx.projectId),
	},
	{
		id: 'automations',
		title: 'Automations',
		icon: Clock,
		defaultOrder: 4,
		render: (ctx) => React.createElement(AutomationsSection, ctx),
	},
	{
		id: 'todos',
		title: 'Todos',
		icon: CheckSquare,
		defaultOrder: 5,
		render: (ctx) => React.createElement(TodosSection, ctx),
	},
	{
		id: 'scratchpads',
		title: 'Scratchpads',
		icon: FileEdit,
		defaultOrder: 6,
		render: (ctx) => React.createElement(ScratchpadsSection, ctx),
	},
	{
		id: 'views',
		title: 'Views',
		icon: LayoutGrid,
		defaultOrder: 7,
		render: (ctx) => React.createElement(ViewsSection, ctx),
		useCount: () => {
			const { views } = usePkgActivityBarEntries();
			return views.length;
		},
	},
];

export const listExplorerSections = (): ExplorerSectionDefinition[] => builtInSections;
