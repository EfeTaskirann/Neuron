// Flat ESLint config (eslint v9). Base setup only; no project-local
// rule overrides yet (per WP-W2-01: "base config, no rule overrides").
// WP-W2-08 may layer in shadcn/Tailwind specifics.
import js from "@eslint/js";
import globals from "globals";
import tseslint from "typescript-eslint";

export default [
  {
    ignores: ["dist/**", "node_modules/**", ".vite/**", "coverage/**"],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: "module",
      globals: {
        ...globals.browser,
      },
    },
  },
];
