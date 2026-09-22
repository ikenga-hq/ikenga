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

import { FilesSection, filesContextMenu } from './sections/files';
import { ArtifactsSection, artifactsContextMenu } from './sections/artifacts';
import { SessionsSection, sessionsContextMenu } from './sections/sessions';
import { NgwaProjectSection, ngwaProjectContextMenu } from './sections/ngwa-project';
import { AutomationsSection, automationsContextMenu } from './sections/automations';
import { TodosSection, todosContextMenu } from './sections/todos';
import { ScratchpadsSection, scratchpadsContextMenu } from './sections/scratchpads';
import { ViewsSection, viewsContextMenu } from './sections/views';

import { useTerminalStore } from '@/terminal/session-store';
import { useGitStatus } from '@/lib/shell/use-git-status';
import { usePkgActivityBarEntries } from '@/lib/pkg/use-activity-bar-entries';

export interface ExplorerSectionContext {
	projectId: string;
}

export interface ContextMenuItemDef {
	id: string;
	label: string;
	run: () => void;
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
	contextMenu?: (ctx: ExplorerSectionContext) => ContextMenuItemDef[];
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
		contextMenu: () => filesContextMenu,
	},
	{
		id: 'artifacts',
		title: 'Artifacts',
		icon: Shapes,
		defaultOrder: 1,
		render: (ctx) => React.createElement(ArtifactsSection, ctx),
		contextMenu: () => artifactsContextMenu,
	},
	{
		id: 'sessions',
		title: 'Sessions',
		icon: TerminalSquare,
		defaultOrder: 2,
		render: (ctx) => React.createElement(SessionsSection, ctx),
		useCount: () => useTerminalStore((s) => s.tabs.length),
		contextMenu: () => sessionsContextMenu,
	},
	{
		id: 'ngwa-project',
		title: 'Ngwa · project',
		icon: Package,
		defaultOrder: 3,
		render: (ctx) => React.createElement(NgwaProjectSection, ctx),
		contextMenu: () => ngwaProjectContextMenu,
	},
	{
		id: 'automations',
		title: 'Automations',
		icon: Clock,
		defaultOrder: 4,
		render: (ctx) => React.createElement(AutomationsSection, ctx),
		contextMenu: () => automationsContextMenu,
	},
	{
		id: 'todos',
		title: 'Todos',
		icon: CheckSquare,
		defaultOrder: 5,
		render: (ctx) => React.createElement(TodosSection, ctx),
		contextMenu: () => todosContextMenu,
	},
	{
		id: 'scratchpads',
		title: 'Scratchpads',
		icon: FileEdit,
		defaultOrder: 6,
		render: (ctx) => React.createElement(ScratchpadsSection, ctx),
		contextMenu: () => scratchpadsContextMenu,
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
		contextMenu: () => viewsContextMenu,
	},
];

export const listExplorerSections = (): ExplorerSectionDefinition[] => builtInSections;
