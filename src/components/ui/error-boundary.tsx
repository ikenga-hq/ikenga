import React, { Component, type ErrorInfo, type ReactNode } from 'react';

interface ErrorBoundaryProps {
	children: ReactNode;
	fallback?: ReactNode | ((error: Error, reset: () => void) => ReactNode);
}

interface ErrorBoundaryState {
	hasError: boolean;
	error: Error | null;
}

export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
	constructor(props: ErrorBoundaryProps) {
		super(props);
		this.state = {
			hasError: false,
			error: null,
		};
	}

	static getDerivedStateFromError(error: Error): ErrorBoundaryState {
		return {
			hasError: true,
			error,
		};
	}

	componentDidCatch(error: Error, errorInfo: ErrorInfo): void {
		console.error('[ErrorBoundary] React render error caught:', error, errorInfo);
	}

	reset = (): void => {
		this.setState({ hasError: false, error: null });
	};

	render(): ReactNode {
		if (this.state.hasError && this.state.error) {
			if (typeof this.props.fallback === 'function') {
				return this.props.fallback(this.state.error, this.reset);
			}
			if (this.props.fallback) {
				return this.props.fallback;
			}

			const errorMessage = this.state.error.message || String(this.state.error);
			const errorStack = this.state.error.stack;

			return (
				<div style={containerStyle}>
					<div style={titleStyle}>Application Error</div>
					<div style={descStyle}>
						An unexpected error occurred in the user interface.
					</div>
					<div style={codeStyle}>
						{errorMessage}
						{errorStack ? `\n\n${errorStack}` : ''}
					</div>
					<div style={actionsStyle}>
						<button type="button" style={primaryButtonStyle} onClick={() => location.reload()}>
							Reload Window
						</button>
						<button type="button" style={secondaryButtonStyle} onClick={this.reset}>
							Try Again
						</button>
					</div>
				</div>
			);
		}

		return this.props.children;
	}
}

const containerStyle: React.CSSProperties = {
	display: 'flex',
	flexDirection: 'column',
	alignItems: 'center',
	justifyContent: 'center',
	height: '100vh',
	width: '100vw',
	boxSizing: 'border-box',
	backgroundColor: '#1a1611',
	color: '#e6ded3',
	fontFamily: 'system-ui, -apple-system, sans-serif',
	padding: '24px',
	textAlign: 'center',
};

const titleStyle: React.CSSProperties = {
	fontSize: '18px',
	fontWeight: 600,
	marginBottom: '8px',
};

const descStyle: React.CSSProperties = {
	fontSize: '14px',
	opacity: 0.8,
	marginBottom: '16px',
	maxWidth: '480px',
};

const codeStyle: React.CSSProperties = {
	fontSize: '12px',
	fontFamily: 'ui-monospace, SFMono-Regular, monospace',
	backgroundColor: '#26201a',
	border: '1px solid #3d352b',
	borderRadius: '6px',
	padding: '12px',
	maxWidth: '640px',
	maxHeight: '240px',
	overflow: 'auto',
	whiteSpace: 'pre-wrap',
	wordBreak: 'break-word',
	textAlign: 'left',
	marginBottom: '20px',
	color: '#d4c9b9',
};

const actionsStyle: React.CSSProperties = {
	display: 'flex',
	gap: '12px',
};

const primaryButtonStyle: React.CSSProperties = {
	padding: '8px 20px',
	fontSize: '13px',
	fontWeight: 500,
	borderRadius: '6px',
	border: '1px solid #57503f',
	backgroundColor: '#3d3429',
	color: '#efe7db',
	cursor: 'pointer',
};

const secondaryButtonStyle: React.CSSProperties = {
	padding: '8px 20px',
	fontSize: '13px',
	fontWeight: 500,
	borderRadius: '6px',
	border: '1px solid #3d352b',
	backgroundColor: '#26201a',
	color: '#b8ac9d',
	cursor: 'pointer',
};
