// The Ngwa kind → icon mapping, shared by every surface that lists `NgwaItem`
// rows (the Installed/Store faceted list, and the Explorer "Ngwa · project"
// section). Split out of `shell/ngwa/ngwa-list.tsx` so a lightweight row list
// (Explorer) doesn't have to pull that surface's facet bar / detail pane /
// scope-ops graph into its bundle just for an icon lookup.

import {
	AppWindow,
	Bot,
	Clock,
	Layers,
	RefreshCw,
	Shield,
	Slash,
	Terminal,
	User,
	Zap,
} from 'lucide-react';
import type { NgwaKind } from '@ikenga/contract';

export function kindIcon(kind: NgwaKind) {
	switch (kind) {
		case 'app':
			return <AppWindow className="h-3.5 w-3.5 flex-none" />;
		case 'engine':
			return <Bot className="h-3.5 w-3.5 flex-none" />;
		case 'tool':
			return <Terminal className="h-3.5 w-3.5 flex-none" />;
		case 'skill':
			return <Zap className="h-3.5 w-3.5 flex-none" />;
		case 'agent':
			return <User className="h-3.5 w-3.5 flex-none" />;
		case 'command':
			return <Slash className="h-3.5 w-3.5 flex-none" />;
		case 'hook':
			return <Shield className="h-3.5 w-3.5 flex-none" />;
		case 'workflow':
			return <RefreshCw className="h-3.5 w-3.5 flex-none" />;
		case 'schedule':
			return <Clock className="h-3.5 w-3.5 flex-none" />;
		default:
			return <Layers className="h-3.5 w-3.5 flex-none" />;
	}
}
