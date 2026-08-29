import { useQuery } from '@tanstack/react-query';
import { useShellStore } from '@/lib/shell/shell-store';
import { pkgSidecarCall } from '@/lib/tauri-cmd';

export type GitFileStatus = 'modified' | 'added' | 'untracked' | 'conflicted';

export interface GitStatusMap {
	files: Map<string, GitFileStatus>;
	dirtyFolders: Set<string>;
}

export function useGitStatus() {
	const activeProjectId = useShellStore((s) => s.activeProjectId);

	return useQuery<GitStatusMap>({
		queryKey: ['git-status', activeProjectId],
		queryFn: async () => {
			const { activeProjectId, projects } = useShellStore.getState();
			const project = projects.find((p) => p.id === activeProjectId);
			if (!project?.root_path) {
				return { files: new Map(), dirtyFolders: new Set() };
			}

			const repoRoot = project.root_path;
			const request = {
				jsonrpc: '2.0',
				id: 1,
				method: 'changes.list',
				params: { repo: repoRoot },
			};

			try {
				let res = await pkgSidecarCall('com.ikenga.git', 'pa-com-ikenga-git-repo', [], {
					stdin: JSON.stringify(request),
					timeoutSecs: 5,
				});

				if (!res.ok) {
					res = await pkgSidecarCall('com.ikenga.git', 'default', [], {
						stdin: JSON.stringify(request),
						timeoutSecs: 5,
					});
				}

				if (!res.ok || !res.stdout) {
					return { files: new Map(), dirtyFolders: new Set() };
				}

				const parsed = JSON.parse(res.stdout);
				const changes = parsed?.result;
				if (!changes || typeof changes !== 'object') {
					return { files: new Map(), dirtyFolders: new Set() };
				}

				const files = new Map<string, GitFileStatus>();
				const dirtyFolders = new Set<string>();

				const addPath = (relOrAbsPath: string, status: GitFileStatus) => {
					const fullPath = relOrAbsPath.startsWith('/')
						? relOrAbsPath
						: `${repoRoot}/${relOrAbsPath}`;

					files.set(fullPath, status);

					// Propagate dirty state up folder hierarchy
					let cur = fullPath;
					while (true) {
						const idx = cur.lastIndexOf('/');
						if (idx <= 0) break;
						cur = cur.slice(0, idx);
						if (cur === repoRoot || !cur.startsWith(repoRoot)) break;
						dirtyFolders.add(cur);
					}
				};

				if (Array.isArray(changes.conflicted)) {
					for (const item of changes.conflicted) {
						if (item.path) addPath(item.path, 'conflicted');
					}
				}
				if (Array.isArray(changes.staged)) {
					for (const item of changes.staged) {
						if (item.path) addPath(item.path, 'added');
					}
				}
				if (Array.isArray(changes.unstaged)) {
					for (const item of changes.unstaged) {
						if (item.path) addPath(item.path, 'modified');
					}
				}
				if (Array.isArray(changes.untracked)) {
					for (const item of changes.untracked) {
						if (item.path) addPath(item.path, 'untracked');
					}
				}

				return { files, dirtyFolders };
			} catch {
				return { files: new Map(), dirtyFolders: new Set() };
			}
		},
		refetchInterval: 5000,
		staleTime: 2000,
	});
}
