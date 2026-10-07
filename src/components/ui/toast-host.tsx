import { AlertTriangle, Info, ShieldAlert } from 'lucide-react';
import {
	FloatingToastChip,
	type FloatingToastChipVariant,
} from '@/components/ui/floating-toast-chip';
import {
	DEFAULT_TOAST_ACTION_TTL_MS,
	DEFAULT_TOAST_TTL_MS,
	type ToastVariant,
	useToastStore,
} from '@/lib/toast';

const CHIP_VARIANT: Record<ToastVariant, FloatingToastChipVariant> = {
	info: 'info',
	error: 'error',
	notice: 'notice',
};

const ICON: Record<ToastVariant, React.ReactNode> = {
	info: <Info />,
	error: <AlertTriangle />,
	notice: <ShieldAlert />,
};

/**
 * Renders the general client toast store (`@/lib/toast`): one pill at a time,
 * bottom-right above the status bar, the rest queued. Mount once in the shell
 * frame (`status-bar.tsx`).
 */
export function ToastHost() {
	const shown = useToastStore((s) => s.queue[0] ?? null);
	const dismiss = useToastStore((s) => s.dismiss);
	if (!shown) return null;
	const action = shown.action;
	return (
		<FloatingToastChip
			key={shown.id}
			anchor="viewport-bottom-right"
			variant={CHIP_VARIANT[shown.variant]}
			icon={ICON[shown.variant]}
			label={shown.label}
			action={
				action
					? {
							label: action.label,
							onClick: () => {
								dismiss(shown.id);
								void action.run();
							},
						}
					: undefined
			}
			onDismiss={() => dismiss(shown.id)}
			ttlMs={shown.ttlMs ?? (action ? DEFAULT_TOAST_ACTION_TTL_MS : DEFAULT_TOAST_TTL_MS)}
		/>
	);
}
