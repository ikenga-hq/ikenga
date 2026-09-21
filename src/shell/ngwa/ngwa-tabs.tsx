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

			<span
				className="ntab opacity-50 cursor-not-allowed"
				role="tab"
				aria-selected={false}
				aria-disabled="true"
				title="Scopes surface (coming in WP-16)"
			>
				<Layers className="h-3.5 w-3.5" />
				<span>Scopes</span>
				<span className="k">3</span>
			</span>

			<span
				className="ntab opacity-50 cursor-not-allowed"
				role="tab"
				aria-selected={false}
				aria-disabled="true"
				title="Health surface (coming in WP-16)"
			>
				<HeartPulse className="h-3.5 w-3.5" />
				<span>Health</span>
				<span className="k">4</span>
			</span>

			<span
				className="ntab opacity-50 cursor-not-allowed"
				role="tab"
				aria-selected={false}
				aria-disabled="true"
				title="Create surface (coming in WP-17)"
			>
				<Plus className="h-3.5 w-3.5" />
				<span>Create</span>
				<span className="k">5</span>
			</span>
		</div>
	);
}
