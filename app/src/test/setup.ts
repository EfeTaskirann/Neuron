// Vitest + jsdom test setup. Registers the @testing-library/jest-dom
// matchers (toBeInTheDocument, toHaveTextContent, ...) globally so
// every `*.test.tsx` file can use them without re-importing.
import "@testing-library/jest-dom/vitest";
