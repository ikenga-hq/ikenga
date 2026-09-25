// WP-40b — D-07 `notifications`: the status-bar bell that fills
// `NotificationsBellSlot` (`src/shell/status-bar.tsx:173`). Trigger + unread
// badge here; the popover body is `./popover.tsx`.
//
// Also the mount point for `NotificationToastBridge`
// (`@/components/ui/floating-toast-chip.tsx`) — a sibling of the `Popover`,
// not inside `PopoverContent`, so it keeps listening (and the badge count
// stays live) while the popover is closed. See that file's header comment
// for why this is the one place `useNotificationsLiveSync()` is called.
//
// designs/system-flows.html?state=notifications

import { useQuery } from '@tanstack/react-query';
import { Bell } from 'lucide-react';
import { useState } from 'react';
import { NotificationToastBridge } from '@/components/ui/floating-toast-chip';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { notificationsUnreadCountQueryOptions } from '@/lib/queries/notifications';
import { NotificationsPopoverContent } from './popover';

export function NotificationsBell() {
	const [open, setOpen] = useState(false);
	const { data } = useQuery(notificationsUnreadCountQueryOptions());
	const count = data?.total ?? 0;
	const label = count > 0 ? `Notifications, ${count} unread` : 'Notifications, none unread';

	return (
		<>
			<Popover open={open} onOpenChange={setOpen}>
				<PopoverTrigger asChild>
					<button
						type="button"
						data-seg="notifications"
						aria-label={label}
						title="Notifications"
						className="relative flex h-5 items-center rounded-[var(--radius-xs)] px-1.5 text-muted-foreground outline-none transition-colors duration-[var(--motion-fast)] ease-[var(--ease-calm)] motion-reduce:transition-none hover:bg-[var(--bg-raised)] hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
					>
						<Bell aria-hidden className="h-3 w-3" />
						{count > 0 && (
							<span
								aria-hidden
								className="absolute -right-0.5 -top-1 min-w-[13px] rounded-full px-[3px] text-center font-mono text-[9px] leading-[13px]"
								style={{ background: 'var(--achievement)', color: 'var(--bg-base)' }}
							>
								{count > 99 ? '99+' : count}
							</span>
						)}
					</button>
				</PopoverTrigger>
				<PopoverContent
					side="bottom"
					align="end"
					sideOffset={6}
					className="w-[380px] max-w-[calc(100vw-16px)] p-0"
				>
					<NotificationsPopoverContent onClose={() => setOpen(false)} />
				</PopoverContent>
			</Popover>
			<NotificationToastBridge />
		</>
	);
}
