// LSP smoke surface — debug-only.
//
// Mounts <CodeEditor language="tsx" lspClient={...}> against the
// com.ikenga.tsserver-lsp sidecar via the Tauri-direct transport, with a
// synthetic Remotion-flavoured TSX buffer. Use this to confirm:
//   - sidecar lazy-spawns on first didOpen
//   - completion + hover round-trip
//   - diagnostics arrive via the linter gutter
//
// Not registered in nav. Open by navigating to /lsp-smoke in dev.
import { createFileRoute } from '@tanstack/react-router';
import { invoke } from '@/lib/transport';
import { listen } from '@/lib/transport';
import { useEffect, useState } from 'react';

import { CodeEditor } from '@ikenga/ui-lib';
import { createLspClient, createTsLspClient, type TsLspClient } from '@ikenga/ui-lib/lsp';
import { createTauriDirectTransport } from '@ikenga/ui-lib/lsp/transports/tauri-direct';

export const Route = createFileRoute('/lsp-smoke')({
	component: LspSmoke,
});

const PKG_ID = 'com.ikenga.tsserver-lsp';
const SIDECAR_NAME = 'pa-com-ikenga-tsserver-lsp-bridge';

const INITIAL_SOURCE = `// LSP smoke — try hovering, autocompleting, and introducing type errors.
import { Composition } from 'remotion';

interface Props {
  title: string;
}

export const Demo: React.FC<Props> = ({ title }) => {
  return <div>{title.toUpperCase()}</div>;
};

export const RemotionRoot = () => (
  <Composition
    id="demo"
    component={Demo}
    durationInFrames={120}
    fps={30}
    width={1920}
    height={1080}
    defaultProps={{ title: 'Hello' }}
  />
);
`;

function LspSmoke() {
	const [src, setSrc] = useState(INITIAL_SOURCE);
	const [status, setStatus] = useState<'idle' | 'connecting' | 'ready' | 'error'>('idle');
	const [statusDetail, setStatusDetail] = useState('');
	const [lspClient, setLspClient] = useState<TsLspClient | undefined>();

	useEffect(() => {
		let cancelled = false;
		let ts: TsLspClient | undefined;
		setStatus('connecting');
		setStatusDetail('');
		try {
			const transport = createTauriDirectTransport({
				invoke,
				listen,
				pkgId: PKG_ID,
				sidecarName: SIDECAR_NAME,
				onError: (err) => {
					if (cancelled) return;
					console.error('[lsp-smoke] transport error', err);
					setStatus('error');
					setStatusDetail(String(err));
				},
			});
			const rpc = createLspClient(transport);
			ts = createTsLspClient(rpc);
			setLspClient(ts);
			ts.initialize(null)
				.then(() => {
					if (!cancelled) setStatus('ready');
				})
				.catch((err) => {
					if (cancelled) return;
					setStatus('error');
					setStatusDetail(String(err));
				});
		} catch (err) {
			if (!cancelled) {
				setStatus('error');
				setStatusDetail(String(err));
			}
		}
		return () => {
			cancelled = true;
			ts?.dispose();
		};
	}, []);

	return (
		<div className="flex h-full w-full flex-col gap-2 p-4">
			<header className="flex items-center justify-between text-xs">
				<div>
					<strong>/lsp-smoke</strong> — <code>{PKG_ID}</code> sidecar bridge
				</div>
				<div className="flex items-center gap-2">
					<span
						className="inline-block h-2 w-2 rounded-full"
						style={{
							backgroundColor:
								status === 'ready'
									? 'var(--success, lime)'
									: status === 'connecting'
										? 'var(--warning, gold)'
										: status === 'error'
											? 'var(--danger, red)'
											: 'var(--fg-muted, gray)',
						}}
					/>
					<span>{status}</span>
					{statusDetail && <span className="text-[var(--fg-muted)]">— {statusDetail}</span>}
				</div>
			</header>
			<div className="flex-1 overflow-hidden rounded-md border border-[var(--border)]">
				<CodeEditor
					value={src}
					onChange={setSrc}
					language="tsx"
					lspClient={lspClient}
					documentUri="inmemory:///lsp-smoke.tsx"
					ariaLabel="LSP smoke buffer"
				/>
			</div>
		</div>
	);
}
