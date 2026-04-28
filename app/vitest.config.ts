import { defineConfig, mergeConfig } from "vitest/config";
import viteConfig from "./vite.config";

// Vitest config inherits from the Vite config so the React plugin and
// path resolution stay in lock-step with `pnpm dev`. The smoke test in
// `src/App.test.tsx` requires jsdom + the global `expect.toBeInTheDocument`
// matcher which is registered by `src/test/setup.ts`.
export default defineConfig((env) =>
  mergeConfig(
    typeof viteConfig === "function" ? viteConfig(env) : viteConfig,
    defineConfig({
      test: {
        environment: "jsdom",
        globals: true,
        setupFiles: ["./src/test/setup.ts"],
        css: true,
        include: ["src/**/*.{test,spec}.{ts,tsx}"],
        restoreMocks: true,
      },
    }),
  ),
);
