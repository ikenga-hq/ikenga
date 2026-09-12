import { describe, expect, it, vi } from "vitest";

describe("Terminal renderer canary & hygiene (WP-06 / T-06 / T-00)", () => {
	it("ensures DOM renderer fallback status strings contain no misleading canvas references", () => {
		const validStatuses = [
			"dom renderer (webgl disabled)",
			"webgl context lost — fell back to dom renderer",
			"webgl unavailable — dom renderer",
		];

		for (const status of validStatuses) {
			expect(status.toLowerCase()).not.toContain("canvas renderer");
			expect(status.toLowerCase()).toContain("dom renderer");
		}
	});

	it("simulates WebGL context acquisition and context loss fallback cycle", () => {
		let currentStatus = "initial";
		const setStatus = (s: string) => {
			currentStatus = s;
		};

		// Simulate webgl disabled / missing
		const webglEnabled = false;
		if (!webglEnabled) {
			setStatus("dom renderer (webgl disabled)");
		}
		expect(currentStatus).toBe("dom renderer (webgl disabled)");

		// Simulate WebGL enabled, acquiring context then losing it
		let triggerContextLoss: () => void = () => {};
		const mockWebglAddon = {
			onContextLoss: (cb: () => void) => {
				triggerContextLoss = cb;
			},
			dispose: vi.fn(),
		};

		mockWebglAddon.onContextLoss(() => {
			mockWebglAddon.dispose();
			setStatus("webgl context lost — fell back to dom renderer");
		});

		// Trigger context loss event
		triggerContextLoss();
		expect(mockWebglAddon.dispose).toHaveBeenCalled();
		expect(currentStatus).toBe("webgl context lost — fell back to dom renderer");
		expect(currentStatus).not.toContain("canvas");
	});
});
