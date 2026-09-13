// Settings → Activity bar
//
// Management UI for the user-pinned activity bar entries: rename / re-icon /
// delete sections, drag-to-reorder pins within a section (and across to a
// different section), unpin individual entries. The activity bar's right-
// click menu is the inline shortcut for the same ops.
//
// Mirrors the projects.tsx layout style: a top-level page with one card per
// section + a section-less group at the bottom + a "New section" affordance.

import { useEffect, useMemo, useState } from 'react';
import { createFileRoute } from '@tanstack/react-router';
import { GripVertical, Pin as PinGlyph, Plus, Trash2, X } from 'lucide-react';
import { create } from 'zustand';

import { Button } from '@/components/ui/button';
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { cn } from '@/components/ui/utils';
import { beginPointerDrag, useDropTarget } from '@/lib/panes/pointer-drag';
import { PinIcon } from '@/shell/pin-icon';
import {
	computeCrossSectionReorderIds,
	computeReorderIds,
	RESERVED_SECTION_IDS,
	slugifySectionId,
	usePinsStore,
	type Pin,
	type PinKind,
	type Section,
} from '@/lib/shell/pins-store';

export const Route = createFileRoute('/settings/activity-bar')({
	component: ActivityBarSettings,
});

const NO_SECTION = '__none__';

function ActivityBarSettings() {
	const sections = usePinsStore((s) => s.sections);
	const pins = usePinsStore((s) => s.pins);
	const hydrated = usePinsStore((s) => s.hydrated);
	const hydrate = usePinsStore((s) => s.hydrate);

	useEffect(() => {
		void hydrate();
	}, [hydrate]);

	const sortedSections = useMemo(
		() =>
			[...sections].sort(
				(a, b) => a.sortOrder - b.sortOrder || a.createdAt.localeCompare(b.createdAt)
			),
		[sections]
	);
	const pinsBySection = useMemo(() => {
		const m = new Map<string, Pin[]>();
		const sortedPins = [...pins].sort(
			(a, b) => a.sortOrder - b.sortOrder || a.createdAt.localeCompare(b.createdAt)
		);
		const sectionLess: Pin[] = [];
		for (const pin of sortedPins) {
			if (pin.sectionId === null) {
				sectionLess.push(pin);
			} else {
				const list = m.get(pin.sectionId) ?? [];
				list.push(pin);
				m.set(pin.sectionId, list);
			}
		}
		return { bySection: m, sectionLess };
	}, [pins]);

	return (
		<div className="mx-auto flex max-w-3xl flex-col gap-6 p-8">
			<header className="flex flex-col gap-1">
				<h1 className="text-2xl font-semibold">Activity bar</h1>
				<p className="text-sm text-muted-foreground">
					Sections group your pinned routes, artifacts, and links in the activity bar. Drag pins to
					reorder within a section or to move them between sections.
				</p>
			</header>

			{!hydrated && <p className="text-sm text-muted-foreground">Loading…</p>}

			{hydrated && (
				<>
					{sortedSections.map((section) => (
						<SectionCard
							key={section.id}
							section={section}
							pins={pinsBySection.bySection.get(section.id) ?? []}
						/>
					))}

					<SectionCard key={NO_SECTION} section={null} pins={pinsBySection.sectionLess} />

					<NewSectionForm existing={sortedSections} />
				</>
			)}
		</div>
	);
}

interface SectionCardProps {
	/** null for the implicit "no section" group. */
	section: Section | null;
	pins: Pin[];
}

function SectionCard({ section, pins }: SectionCardProps) {
	const [editingLabel, setEditingLabel] = useState(false);
	const [draftLabel, setDraftLabel] = useState(section?.label ?? '');
	const [editingIcon, setEditingIcon] = useState(false);
	const [draftIconLucide, setDraftIconLucide] = useState(section?.iconLucide ?? '');
	const [draftIconEmoji, setDraftIconEmoji] = useState(section?.iconEmoji ?? '');
	const [confirmDelete, setConfirmDelete] = useState(false);

	const updateSection = usePinsStore((s) => s.updateSection);
	const removeSection = usePinsStore((s) => s.removeSection);

	useEffect(() => {
		setDraftLabel(section?.label ?? '');
		setDraftIconLucide(section?.iconLucide ?? '');
		setDraftIconEmoji(section?.iconEmoji ?? '');
	}, [section]);

	const isVirtual = section === null;
	const sectionId = isVirtual ? NO_SECTION : section.id;

	async function commitLabel() {
		if (isVirtual) return;
		const trimmed = draftLabel.trim();
		if (!trimmed || trimmed === section.label) {
			setEditingLabel(false);
			setDraftLabel(section.label);
			return;
		}
		try {
			await updateSection({ id: section.id, label: trimmed });
		} catch {
			// Revert on failure; the pins-store already pushed the error.
			setDraftLabel(section.label);
		}
		setEditingLabel(false);
	}

	async function commitIcon() {
		if (isVirtual) return;
		try {
			await updateSection({
				id: section.id,
				iconLucide: draftIconLucide.trim() || null,
				iconEmoji: draftIconEmoji.trim() || null,
			});
		} catch {
			setDraftIconLucide(section.iconLucide ?? '');
			setDraftIconEmoji(section.iconEmoji ?? '');
		}
		setEditingIcon(false);
	}

	async function handleDelete() {
		if (isVirtual) return;
		try {
			await removeSection(section.id);
		} catch {
			// fall through; error surfaces via store
		}
		setConfirmDelete(false);
	}

	return (
		<section
			className="rounded-lg border border-border bg-card"
			data-section-id={sectionId}
			aria-label={isVirtual ? 'No section' : `Section ${section.label}`}
		>
			<div className="flex items-center gap-2 border-b border-border px-4 py-3">
				{!isVirtual && section.iconEmoji ? (
					<span className="text-lg leading-none">{section.iconEmoji}</span>
				) : null}
				{editingLabel && !isVirtual ? (
					<Input
						autoFocus
						value={draftLabel}
						onChange={(e) => setDraftLabel(e.target.value)}
						onBlur={commitLabel}
						onKeyDown={(e) => {
							if (e.key === 'Enter') {
								e.preventDefault();
								void commitLabel();
							} else if (e.key === 'Escape') {
								setDraftLabel(section.label);
								setEditingLabel(false);
							}
						}}
						className="h-7 max-w-xs text-sm"
					/>
				) : (
					<button
						type="button"
						onClick={() => !isVirtual && setEditingLabel(true)}
						disabled={isVirtual}
						className={cn('flex-1 text-left text-sm font-medium', !isVirtual && 'hover:underline')}
						title={isVirtual ? undefined : 'Click to rename'}
					>
						{isVirtual ? 'No section' : section.label}
						{!isVirtual && (
							<span className="ml-2 text-[11px] font-normal text-muted-foreground">
								(id: {section.id})
							</span>
						)}
					</button>
				)}
				<span className="text-xs text-muted-foreground">
					{pins.length} {pins.length === 1 ? 'pin' : 'pins'}
				</span>
				{!isVirtual && (
					<>
						<Button
							type="button"
							variant="ghost"
							size="sm"
							onClick={() => setEditingIcon((v) => !v)}
						>
							Icon
						</Button>
						<Button
							type="button"
							variant="ghost"
							size="sm"
							className="text-destructive hover:text-destructive"
							onClick={() => setConfirmDelete(true)}
							aria-label={`Delete section ${section.label}`}
						>
							<Trash2 className="h-3.5 w-3.5" />
						</Button>
					</>
				)}
			</div>

			{editingIcon && !isVirtual && (
				<div className="flex items-end gap-3 border-b border-border bg-muted/30 px-4 py-3">
					<label className="flex flex-col gap-1">
						<span className="text-[11px] font-medium text-muted-foreground">Icon (lucide)</span>
						<Input
							value={draftIconLucide}
							onChange={(e) => setDraftIconLucide(e.target.value)}
							placeholder="banknote"
							className="h-7 w-44 text-sm"
						/>
					</label>
					<label className="flex flex-col gap-1">
						<span className="text-[11px] font-medium text-muted-foreground">Emoji</span>
						<Input
							value={draftIconEmoji}
							onChange={(e) => setDraftIconEmoji(e.target.value)}
							placeholder="💰"
							maxLength={4}
							className="h-7 w-20 text-sm"
						/>
					</label>
					<Button type="button" size="sm" onClick={commitIcon}>
						Save
					</Button>
					<Button
						type="button"
						size="sm"
						variant="ghost"
						onClick={() => {
							setDraftIconLucide(section.iconLucide ?? '');
							setDraftIconEmoji(section.iconEmoji ?? '');
							setEditingIcon(false);
						}}
					>
						Cancel
					</Button>
				</div>
			)}

			<PinList sectionId={isVirtual ? null : section.id} pins={pins} />

			<NewPinForm sectionId={isVirtual ? null : section.id} />

			{!isVirtual && confirmDelete && (
				<DeleteConfirmDialog
					section={section}
					pinCount={pins.length}
					onCancel={() => setConfirmDelete(false)}
					onConfirm={handleDelete}
				/>
			)}
		</section>
	);
}

interface PinListProps {
	sectionId: string | null;
	pins: Pin[];
}

interface PinDrag {
	pinId: string;
	fromSectionId: string | null;
}

// The pin being dragged. Shared by every section's list (not per-list state)
// so a pin can be dropped into a different section. Drags run on pointer
// events, not HTML5 DnD, which WebView2 blocks on Windows — see
// `lib/panes/pointer-drag.ts`.
const usePinDrag = create<{ drag: PinDrag | null; setDrag: (d: PinDrag | null) => void }>(
	(set) => ({ drag: null, setDrag: (drag) => set({ drag }) })
);

/** Insertion index for a pointer at client `y` over a list's pin rows. */
function insertIndexAt(listEl: Element, y: number): number {
	const rows = Array.from(listEl.querySelectorAll<HTMLElement>('li[data-pin-idx]'));
	for (const row of rows) {
		const r = row.getBoundingClientRect();
		if (y < r.top + r.height / 2) return Number(row.dataset.pinIdx);
	}
	return rows.length;
}

function PinList({ sectionId, pins }: PinListProps) {
	const removePin = usePinsStore((s) => s.removePin);
	const reorderPins = usePinsStore((s) => s.reorderPins);
	const drag = usePinDrag((s) => s.drag);
	const [hoverIdx, setHoverIdx] = useState<number | null>(null);

	const sectionKey = sectionId ?? '';

	const listDrop = useDropTarget({
		accepts: () => usePinDrag.getState().drag !== null,
		onOver: (_x, y, el) => {
			const next = insertIndexAt(el, y);
			setHoverIdx((prev) => (prev === next ? prev : next));
		},
		onLeave: () => setHoverIdx(null),
		onDrop: (_x, y, el) => {
			const dropped = usePinDrag.getState().drag;
			setHoverIdx(null);
			if (!dropped) return;
			const dstIdx = insertIndexAt(el, y);
			if (dropped.fromSectionId === sectionId) {
				const srcIdx = pins.findIndex((p) => p.id === dropped.pinId);
				if (srcIdx < 0) return;
				// Same-position no-op (drop adjacent to self with no movement).
				if (srcIdx === dstIdx || srcIdx + 1 === dstIdx) return;
				void commitDrop(srcIdx, dstIdx);
			} else {
				void commitCrossSectionDrop(dropped.pinId, dstIdx);
			}
		},
	});

	if (pins.length === 0) {
		return (
			<EmptyDropZone
				sectionId={sectionId}
				onDropPin={async (pinId, fromSectionId) => {
					if (fromSectionId === sectionId) return;
					await reorderPins([pinId], sectionKey);
				}}
			/>
		);
	}

	async function commitDrop(srcIdx: number, dstIdx: number) {
		const ids = computeReorderIds(pins, srcIdx, dstIdx);
		if (ids.length === 0) return;
		await reorderPins(ids, sectionKey);
	}

	async function commitCrossSectionDrop(pinId: string, dstIdx: number) {
		const ids = computeCrossSectionReorderIds(pins, pinId, dstIdx);
		await reorderPins(ids, sectionKey);
	}

	return (
		<ul {...listDrop} className="flex flex-col" data-section-key={sectionKey}>
			{pins.map((pin, idx) => {
				const isDragging = drag?.pinId === pin.id;
				const showInsertBefore = hoverIdx === idx && drag !== null && !isDragging;
				return (
					<li
						key={pin.id}
						data-pin-idx={idx}
						onPointerDown={(e) => {
							// The unpin button is a click, not a drag handle.
							if ((e.target as Element).closest('button')) return;
							beginPointerDrag(e, {
								label: pin.label,
								onStart: () =>
									usePinDrag.getState().setDrag({ pinId: pin.id, fromSectionId: pin.sectionId }),
								onEnd: () => {
									usePinDrag.getState().setDrag(null);
									setHoverIdx(null);
								},
							});
						}}
						className={cn(
							'group relative flex select-none items-center gap-3 px-4 py-2 transition-colors',
							idx > 0 && 'border-t border-border/40',
							isDragging && 'opacity-40',
							showInsertBefore &&
								'before:absolute before:left-0 before:right-0 before:top-0 before:h-0.5 before:bg-primary'
						)}
					>
						<GripVertical className="h-4 w-4 cursor-grab text-muted-foreground/60" aria-hidden />
						<div className="grid h-7 w-7 shrink-0 place-items-center rounded bg-muted text-muted-foreground">
							<PinIcon iconLucide={pin.iconLucide} iconEmoji={pin.iconEmoji} Fallback={PinGlyph} />
						</div>
						<div className="flex flex-1 flex-col">
							<span className="truncate text-sm">{pin.label}</span>
							<span className="truncate text-[11px] text-muted-foreground">
								{describePinTarget(pin)}
							</span>
						</div>
						<Button
							type="button"
							variant="ghost"
							size="sm"
							onClick={() => removePin(pin.id)}
							aria-label={`Unpin ${pin.label}`}
							className="opacity-0 group-hover:opacity-100"
						>
							<X className="h-3.5 w-3.5" />
						</Button>
					</li>
				);
			})}
			{/* Tail spacer — only shown while dragging so it doesn't take up extra
			    space at rest. It sits inside the list's drop target, so a pointer
			    below the last pin inserts at the end (hover `idx === pins.length`). */}
			{drag !== null && (
				<li
					data-tail-drop
					className={cn(
						'h-3 transition-colors',
						hoverIdx === pins.length && 'border-t-2 border-primary'
					)}
				/>
			)}
		</ul>
	);
}

interface EmptyDropZoneProps {
	sectionId: string | null;
	onDropPin: (pinId: string, fromSectionId: string | null) => void | Promise<void>;
}

function EmptyDropZone({ sectionId, onDropPin }: EmptyDropZoneProps) {
	const [hover, setHover] = useState(false);
	const dropTarget = useDropTarget({
		accepts: () => usePinDrag.getState().drag !== null,
		onOver: () => setHover(true),
		onLeave: () => setHover(false),
		onDrop: () => {
			const dropped = usePinDrag.getState().drag;
			setHover(false);
			if (dropped) void onDropPin(dropped.pinId, dropped.fromSectionId);
		},
	});
	return (
		<div
			{...dropTarget}
			className={cn(
				'flex items-center justify-center px-4 py-6 text-xs text-muted-foreground transition-colors',
				hover && 'bg-accent/40 text-accent-foreground'
			)}
			data-empty-drop-zone={sectionId ?? '__none__'}
		>
			{hover
				? 'Drop to move pin here'
				: 'No pins yet — add one below, or pin an artifact from its address bar.'}
		</div>
	);
}

interface NewSectionFormProps {
	existing: readonly Section[];
}

function NewSectionForm({ existing }: NewSectionFormProps) {
	const [open, setOpen] = useState(false);
	const [label, setLabel] = useState('');
	const [submitting, setSubmitting] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const createSection = usePinsStore((s) => s.createSection);

	const slug = slugifySectionId(label);
	const collides = useMemo(
		() => Boolean(slug) && existing.some((s) => s.id === slug),
		[slug, existing]
	);
	const isReserved = RESERVED_SECTION_IDS.includes(slug);

	if (!open) {
		return (
			<Button
				type="button"
				variant="ghost"
				className="self-start text-muted-foreground"
				onClick={() => setOpen(true)}
			>
				<Plus className="mr-1 h-4 w-4" />
				New section
			</Button>
		);
	}

	async function submit(e: React.FormEvent) {
		e.preventDefault();
		const trimmed = label.trim();
		if (!trimmed) {
			setError('Label is required.');
			return;
		}
		if (!slug) {
			setError('Label must contain at least one letter or digit.');
			return;
		}
		if (isReserved) {
			setError(`'${slug}' is a reserved id (host-owned).`);
			return;
		}
		if (collides) {
			setError(`Section id '${slug}' already exists.`);
			return;
		}
		setSubmitting(true);
		setError(null);
		try {
			await createSection({ id: slug, label: trimmed });
			setLabel('');
			setOpen(false);
		} catch (err) {
			setError(err instanceof Error ? err.message : String(err));
		} finally {
			setSubmitting(false);
		}
	}

	return (
		<form
			onSubmit={submit}
			className="flex flex-col gap-2 rounded-lg border border-dashed border-border p-4"
		>
			<label className="flex flex-col gap-1">
				<span className="text-xs font-medium text-muted-foreground">New section label</span>
				<Input
					autoFocus
					value={label}
					onChange={(e) => {
						setLabel(e.target.value);
						if (error) setError(null);
					}}
					placeholder="e.g. Finance"
				/>
				{slug && (
					<span className="text-[11px] text-muted-foreground">
						id: <code className="font-mono">{slug}</code>
						{collides && <span className="ml-2 text-destructive">already exists</span>}
						{isReserved && <span className="ml-2 text-destructive">reserved id</span>}
					</span>
				)}
			</label>
			{error && (
				<div className="rounded border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
					{error}
				</div>
			)}
			<div className="flex gap-2">
				<Button type="submit" size="sm" disabled={submitting}>
					{submitting ? 'Creating…' : 'Create section'}
				</Button>
				<Button
					type="button"
					size="sm"
					variant="ghost"
					onClick={() => {
						setLabel('');
						setError(null);
						setOpen(false);
					}}
				>
					Cancel
				</Button>
			</div>
		</form>
	);
}

interface NewPinFormProps {
	sectionId: string | null;
}

const PIN_KIND_OPTIONS: Array<{ value: PinKind; label: string; placeholder: string }> = [
	{ value: 'route', label: 'Route', placeholder: '/scratchpads' },
	{ value: 'external', label: 'External URL', placeholder: 'https://example.com' },
	{ value: 'file', label: 'File', placeholder: '/absolute/path/to/file.html' },
];

function NewPinForm({ sectionId }: NewPinFormProps) {
	const addPin = usePinsStore((s) => s.addPin);
	const [open, setOpen] = useState(false);
	const [kind, setKind] = useState<PinKind>('route');
	const [label, setLabel] = useState('');
	const [target, setTarget] = useState('');
	const [iconLucide, setIconLucide] = useState('');
	const [iconEmoji, setIconEmoji] = useState('');
	const [submitting, setSubmitting] = useState(false);
	const [error, setError] = useState<string | null>(null);

	function reset() {
		setKind('route');
		setLabel('');
		setTarget('');
		setIconLucide('');
		setIconEmoji('');
		setError(null);
	}

	if (!open) {
		return (
			<div className="border-t border-border px-4 py-2">
				<Button
					type="button"
					variant="ghost"
					size="sm"
					className="text-muted-foreground"
					onClick={() => setOpen(true)}
				>
					<Plus className="mr-1 h-3.5 w-3.5" />
					New pin
				</Button>
			</div>
		);
	}

	async function submit(e: React.FormEvent) {
		e.preventDefault();
		const trimmedLabel = label.trim();
		const trimmedTarget = target.trim();
		if (!trimmedLabel) {
			setError('Label is required.');
			return;
		}
		if (!trimmedTarget) {
			setError('Target is required.');
			return;
		}
		if (kind === 'route' && !trimmedTarget.startsWith('/')) {
			setError('Route targets must start with "/".');
			return;
		}
		if (kind === 'external' && !/^https?:\/\//i.test(trimmedTarget)) {
			setError('External targets must start with http:// or https://.');
			return;
		}
		if (kind === 'file' && !trimmedTarget.startsWith('/')) {
			setError('File targets must be absolute paths.');
			return;
		}
		setSubmitting(true);
		setError(null);
		try {
			await addPin({
				kind,
				target: trimmedTarget,
				label: trimmedLabel,
				iconLucide: iconLucide.trim() || null,
				iconEmoji: iconEmoji.trim() || null,
				sectionId,
			});
			reset();
			setOpen(false);
		} catch (err) {
			setError(err instanceof Error ? err.message : String(err));
		} finally {
			setSubmitting(false);
		}
	}

	const placeholder = PIN_KIND_OPTIONS.find((o) => o.value === kind)?.placeholder ?? '';

	return (
		<form
			onSubmit={submit}
			className="flex flex-col gap-3 border-t border-border bg-muted/30 px-4 py-3"
		>
			<div className="flex flex-wrap items-end gap-3">
				<label className="flex flex-col gap-1">
					<span className="text-[11px] font-medium text-muted-foreground">Kind</span>
					<select
						value={kind}
						onChange={(e) => setKind(e.target.value as PinKind)}
						className="h-8 rounded border border-input bg-background px-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring"
					>
						{PIN_KIND_OPTIONS.map((o) => (
							<option key={o.value} value={o.value}>
								{o.label}
							</option>
						))}
					</select>
				</label>
				<label className="flex flex-1 flex-col gap-1" style={{ minWidth: 200 }}>
					<span className="text-[11px] font-medium text-muted-foreground">Label</span>
					<Input
						autoFocus
						value={label}
						onChange={(e) => {
							setLabel(e.target.value);
							if (error) setError(null);
						}}
						placeholder="Scratchpads"
						className="h-8 text-sm"
					/>
				</label>
			</div>
			<label className="flex flex-col gap-1">
				<span className="text-[11px] font-medium text-muted-foreground">Target</span>
				<Input
					value={target}
					onChange={(e) => {
						setTarget(e.target.value);
						if (error) setError(null);
					}}
					placeholder={placeholder}
					className="h-8 font-mono text-sm"
				/>
			</label>
			<div className="flex flex-wrap items-end gap-3">
				<label className="flex flex-col gap-1">
					<span className="text-[11px] font-medium text-muted-foreground">Icon (lucide name)</span>
					<Input
						value={iconLucide}
						onChange={(e) => setIconLucide(e.target.value)}
						placeholder="file-text"
						className="h-8 w-44 text-sm"
					/>
				</label>
				<label className="flex flex-col gap-1">
					<span className="text-[11px] font-medium text-muted-foreground">Emoji</span>
					<Input
						value={iconEmoji}
						onChange={(e) => setIconEmoji(e.target.value)}
						placeholder="📝"
						maxLength={4}
						className="h-8 w-20 text-sm"
					/>
				</label>
			</div>
			{error && (
				<div className="rounded border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
					{error}
				</div>
			)}
			<div className="flex gap-2">
				<Button type="submit" size="sm" disabled={submitting}>
					{submitting ? 'Adding…' : 'Add pin'}
				</Button>
				<Button
					type="button"
					size="sm"
					variant="ghost"
					onClick={() => {
						reset();
						setOpen(false);
					}}
				>
					Cancel
				</Button>
			</div>
		</form>
	);
}

interface DeleteConfirmDialogProps {
	section: Section;
	pinCount: number;
	onCancel: () => void;
	onConfirm: () => void;
}

function DeleteConfirmDialog({ section, pinCount, onCancel, onConfirm }: DeleteConfirmDialogProps) {
	return (
		<Dialog open onOpenChange={(o) => !o && onCancel()}>
			<DialogContent className="sm:max-w-md">
				<DialogHeader>
					<DialogTitle>Delete section "{section.label}"?</DialogTitle>
					<DialogDescription>
						{pinCount === 0 ? (
							<>This section has no pins. It will be removed.</>
						) : (
							<>
								Its {pinCount} {pinCount === 1 ? 'pin' : 'pins'} will move to{' '}
								<strong>No section</strong> — they won't be deleted.
							</>
						)}
					</DialogDescription>
				</DialogHeader>
				<DialogFooter>
					<Button type="button" variant="ghost" onClick={onCancel}>
						Cancel
					</Button>
					<Button
						type="button"
						className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
						onClick={onConfirm}
					>
						Delete section
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}

/** Short subtitle showing the pin's target. Truncates long paths and URLs
 *  for readability. */
function describePinTarget(pin: Pin): string {
	const max = 64;
	const target = pin.target;
	if (target.length <= max) return target;
	return `…${target.slice(-(max - 1))}`;
}
