import { createContext, useContext, type ReactNode } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { FileText, RotateCcw } from 'lucide-react';

import { Button } from '@/components/ui/button';
import { cn } from '@/components/ui/utils';
import type { SettingsScopeId } from '@/shell/settings/nav';
import {
	readSettingsFile,
	watchSettings,
	writeSettingsField,
	type SettingsFileResult,
} from '@/lib/settings/client';

export interface SettingsSectionContextValue {
	scope: SettingsScopeId;
	setScope: (scope: SettingsScopeId) => void;
	projectId: string | null;
	projectRoot: string | null;
	result: SettingsFileResult | undefined;
	overrides: ReadonlySet<string>;
	isLoading: boolean;
	refresh: () => void;
}

const SettingsSectionContext = createContext<SettingsSectionContextValue | null>(null);

export const SettingsSectionProvider = SettingsSectionContext.Provider;

export function useSettingsSection(): SettingsSectionContextValue {
	const value = useContext(SettingsSectionContext);
	if (!value) {
		throw new Error('useSettingsSection must be used inside the settings shell');
	}
	return value;
}

export function useSettingsDocument(scope: SettingsScopeId, projectId: string | null) {
	const queryClient = useQueryClient();
	const query = useQuery({
		queryKey: ['settings', 'file', scope, projectId],
		queryFn: () => readSettingsFile({ scope, projectId }),
		staleTime: 15_000,
	});
	void watchSettings(() => {
		queryClient.invalidateQueries({ queryKey: ['settings', 'file'] });
	});
	return {
		result: query.data,
		isLoading: query.isLoading,
		refresh: () => queryClient.invalidateQueries({ queryKey: ['settings', 'file'] }),
	};
}

export function useRevertOverride() {
	const { projectId, refresh } = useSettingsSection();
	return useMutation({
		mutationFn: async (field: string) => {
			await writeSettingsField({ scope: 'project', field, value: null, remove: true, projectId });
		},
		onSuccess: () => refresh(),
	});
}

interface SettingsFieldRowProps {
	field: string | null;
	label: string;
	desc?: ReactNode;
	stacked?: boolean;
	children: ReactNode;
	className?: string;
}

export function SettingsFieldRow({
	field,
	label,
	desc,
	stacked,
	children,
	className,
}: SettingsFieldRowProps) {
	const { scope, overrides } = useSettingsSection();
	const revert = useRevertOverride();
	const isOverride = field !== null && scope === 'project' && overrides.has(field);
	return (
		<div
			data-field={field ?? undefined}
			className={cn(
				'px-4 py-3',
				stacked ? 'space-y-3' : 'grid grid-cols-[1fr_auto] items-center gap-4',
				isOverride && 'bg-primary/[0.04]',
				className
			)}
		>
			<div className="min-w-0 space-y-0.5">
				<div className="flex items-center gap-2 text-sm font-medium text-foreground">
					<span className="truncate">{label}</span>
				</div>
				{desc && <div className="text-xs leading-relaxed text-muted-foreground">{desc}</div>}
				{isOverride && (
					<div className="flex items-center gap-2 pt-0.5">
						<span className="inline-flex items-center gap-1 font-mono text-[10px] uppercase tracking-wider text-primary">
							<FileText className="h-3 w-3" />
							project override
						</span>
						<Button
							variant="ghost"
							size="sm"
							className="h-5 gap-1 rounded px-1.5 text-[10px] text-muted-foreground hover:text-foreground"
							onClick={() => revert.mutate(field)}
							disabled={revert.isPending}
							aria-label={`Revert ${label} to the personal value`}
						>
							<RotateCcw className="h-2.5 w-2.5" />
							Revert
						</Button>
					</div>
				)}
			</div>
			<div className={cn(stacked ? 'w-full' : 'shrink-0')}>{children}</div>
		</div>
	);
}
