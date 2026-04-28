/// <reference types="vite/client" />

// Side-effect-only CSS imports. Vite handles bundling at runtime;
// TypeScript just needs to know the import is valid.
declare module "*.css";
