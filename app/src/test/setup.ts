// Vitest + jsdom test setup. Registers the @testing-library/jest-dom
// matchers (toBeInTheDocument, toHaveTextContent, ...) globally so
// every `*.test.tsx` file can use them without re-importing.
import "@testing-library/jest-dom/vitest";

// jsdom doesn't ship ResizeObserver. xterm's pane body uses one to
// refit on layout changes; in tests the observer never fires
// anything meaningful, so a no-op stub is enough.
if (typeof globalThis.ResizeObserver === "undefined") {
  (globalThis as unknown as { ResizeObserver: unknown }).ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
}
