// Markdown formatting controls — the extras slot of the shared editing
// toolbar (src/viewer/editing/editor-toolbar.tsx), shown while editing. Owns
// no editor state: the parent implements each action against the live
// CodeMirror view. Edit / Done / Save / Cancel and the dirty dot now live in
// the shared toolbar.

import {
	Bold,
	Code,
	Heading,
	Italic,
	Link as LinkIcon,
	List,
	Loader2,
	Quote,
	WandSparkles,
} from 'lucide-react';

interface MarkdownFormatControlsProps {
	formatting: boolean;
	/** Another buffer operation is running (or nothing is editable). */
	formatDisabled?: boolean;
	onFormatDoc: () => void;
	onWrap: (before: string, after?: string) => void;
	onPrefix: (prefix: string) => void;
	onLink: () => void;
}

export function MarkdownFormatControls({
	formatting,
	formatDisabled,
	onFormatDoc,
	onWrap,
	onPrefix,
	onLink,
}: MarkdownFormatControlsProps) {
	return (
		<>
			<Divider />
			<IconButton title="Bold (⌘B)" onClick={() => onWrap('**')}>
				<Bold className="h-3.5 w-3.5" />
			</IconButton>
			<IconButton title="Italic (⌘I)" onClick={() => onWrap('_')}>
				<Italic className="h-3.5 w-3.5" />
			</IconButton>
			<IconButton title="Inline code" onClick={() => onWrap('`')}>
				<Code className="h-3.5 w-3.5" />
			</IconButton>
			<IconButton title="Heading" onClick={() => onPrefix('## ')}>
				<Heading className="h-3.5 w-3.5" />
			</IconButton>
			<IconButton title="Bullet list" onClick={() => onPrefix('- ')}>
				<List className="h-3.5 w-3.5" />
			</IconButton>
			<IconButton title="Quote" onClick={() => onPrefix('> ')}>
				<Quote className="h-3.5 w-3.5" />
			</IconButton>
			<IconButton title="Link" onClick={onLink}>
				<LinkIcon className="h-3.5 w-3.5" />
			</IconButton>
			<Divider />
			<IconButton
				title="Format document"
				onClick={onFormatDoc}
				disabled={formatting || formatDisabled}
			>
				{formatting ? (
					<Loader2 className="h-3.5 w-3.5 animate-spin" />
				) : (
					<WandSparkles className="h-3.5 w-3.5" />
				)}
			</IconButton>
		</>
	);
}

function IconButton({
	onClick,
	title,
	disabled,
	children,
}: {
	onClick: () => void;
	title: string;
	disabled?: boolean;
	children: React.ReactNode;
}) {
	return (
		<button
			type="button"
			onClick={onClick}
			title={title}
			aria-label={title}
			disabled={disabled}
			className="inline-flex h-7 w-7 items-center justify-center rounded text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50"
		>
			{children}
		</button>
	);
}

function Divider() {
	return <span className="mx-1 h-4 w-px bg-border" aria-hidden="true" />;
}
