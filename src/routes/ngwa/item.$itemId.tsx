// /ngwa/item/$itemId — Full-pane Ngwa equipment detail route (WP-17 / locked D-08).
//
// Mounts NgwaItemDetailSurface for the selected equipment item.

import { createFileRoute, useNavigate, useParams } from '@tanstack/react-router';
import { useNgwaSnapshot } from '@/lib/ngwa/use-ngwa-snapshot';
import { NgwaItemDetailSurface } from '@/shell/ngwa/ngwa-item-detail-surface';
import { ArrowLeft } from 'lucide-react';
import '@/shell/ngwa/ngwa.css';

function NgwaItemDetailPage() {
	const { itemId } = useParams({ from: '/ngwa/item/$itemId' });
	const navigate = useNavigate();
	const { items, isLoading, error } = useNgwaSnapshot();

	// Match by id or by name (for clean human-readable navigation)
	const item = items.find((i) => i.id === itemId || i.name === itemId);

	function handleBack() {
		void navigate({ to: '/ngwa/installed' });
	}

	if (isLoading) {
		return (
			<div className="view-ngwa flex-1 min-h-0 flex flex-col p-6">
				<div className="empty">
					<span className="emberbar">
						<i />
						Loading equipment details...
					</span>
				</div>
			</div>
		);
	}

	if (error) {
		return (
			<div className="view-ngwa flex-1 min-h-0 flex flex-col p-6">
				<div className="empty text-destructive">
					Failed to load equipment: {String(error)}
				</div>
			</div>
		);
	}

	if (!item) {
		return (
			<div className="view-ngwa flex-1 min-h-0 flex flex-col p-6">
				<div className="empty">
					<p className="mb-3">No equipment item found matching “{itemId}”.</p>
					<button type="button" className="btn" onClick={handleBack}>
						<ArrowLeft className="h-3.5 w-3.5 mr-1.5" /> Back to catalogue
					</button>
				</div>
			</div>
		);
	}

	return (
		<NgwaItemDetailSurface
			item={item}
			onBack={handleBack}
		/>
	);
}

export const Route = createFileRoute('/ngwa/item/$itemId')({
	component: NgwaItemDetailPage,
});
