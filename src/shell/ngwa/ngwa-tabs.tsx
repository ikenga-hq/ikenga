// Ngwa Top-Level Tab Strip (WP-15 / locked D-02 frame-workbench-v4.html)
//
// Surfaces: Installed (1) · Store (2) · Scopes (3) · Health (4) · Create (5)
// Scopes and Health are WP-16; Create is WP-17.

import { Link } from '@tanstack/react-router';
import { Package, Store, Layers, HeartPulse, Plus } from 'lucide-react';

export interface NgwaTabsProps {
	activeTab: 'installed' | 'store' | 'scopes' | 'health' | 'create';
	installedCount?: number;
}

export function NgwaTabs({ activeTab, installedCount }: NgwaTabsProps) {
	return (
		<div className="ntabs" role="tablist" aria-label="Ngwa surfaces">
			<Link
				to="/ngwa/installed"
				className={`ntab ${activeTab === 'installed' ? 'on' : ''}`}
				role="tab"
				aria-selected={activeTab === 'installed'}
			>
				<Package className="h-3.5 w-3.5" />
				<span>Installed</span>
				{installedCount !== undefined && <span className="cnt" data-instcount>{installedCount}</span>}
				<span className="k">1</span>
			</Link>

			<Link
				to="/ngwa/store"
				className={`ntab ${activeTab === 'store' ? 'on' : ''}`}
				role="tab"
				aria-selected={activeTab === 'store'}
			>
				<Store className="h-3.5 w-3.5" />
				<span>Store</span>
				<span className="k">2</span>
			</Link>

			<Link
				to="/ngwa/scopes"
				className={`ntab ${activeTab === 'scopes' ? 'on' : ''}`}
				role="tab"
				aria-selected={activeTab === 'scopes'}
			>
				<Layers className="h-3.5 w-3.5" />
				<span>Scopes</span>
				<span className="k">3</span>
			</Link>

			<Link
				to="/ngwa/health"
				className={`ntab ${activeTab === 'health' ? 'on' : ''}`}
				role="tab"
				aria-selected={activeTab === 'health'}
			>
				<HeartPulse className="h-3.5 w-3.5" />
				<span>Health</span>
				<span className="k">4</span>
			</Link>

			<Link
				to="/ngwa/create"
				className={`ntab ${activeTab === 'create' ? 'on' : ''}`}
				role="tab"
				aria-selected={activeTab === 'create'}
			>
				<Plus className="h-3.5 w-3.5" />
				<span>Create</span>
				<span className="k">5</span>
			</Link>
		</div>
	);
}
