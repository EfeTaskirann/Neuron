---
id: WP-W2-01
title: Tauri 2 scaffold
owner: TBD
status: not-started
depends-on: []
acceptance-gate: "pnpm tauri dev opens a window with a React 'Hello Neuron' page in dark mode; smoke test passes via pnpm test --run"
---

## Goal

Bootstrap a Tauri 2 + React 18 + TypeScript project at the repo root such that `pnpm tauri dev` opens a desktop window rendering a placeholder React page. No backend logic, no DB, no design-system migration yet. Establish the test harness (Vitest + Testing Library) so that the Charter's `pnpm test --run` gate has a real surface from WP-01 onward.

## Scope (this WP only)

- Initialize `app/` (Vite + React 18 + TS) at repo root
- Initialize `src-tauri/` (Tauri 2 Rust crate) at repo root
- Configure `tauri.conf.json`: window title "Neuron", default 1280×800, dev URL `http://localhost:5173`
- Wire pnpm scripts at root: `dev`, `tauri:dev`, `build`, `tauri:build`, `typecheck`, `lint`, `test`
- Set up `pnpm-workspace.yaml` listing `app` (workspace ready for sub-packages)
- Set up `Cargo.toml` workspace at root listing `["src-tauri"]`
- Add a single React page `app/src/App.tsx` that renders "Hello Neuron"
- Apply dark mode by importing `Neuron Design/colors_and_type.css` (transient — moved to `app/src/styles/` in WP-W2-08)
- **Test harness**: install Vitest + @testing-library/react + @testing-library/jest-dom + jsdom; add `app/vitest.config.ts`, `app/src/test/setup.ts`, and one smoke test `app/src/App.test.tsx` that verifies (a) the "Hello Neuron" string renders and (b) the surface background CSS variable resolves to the OKLCH midnight-950 token

## Out of scope

- Tauri commands (WP-W2-03)
- Database (WP-W2-02)
- Any UI beyond a single placeholder page
- TanStack Query setup (WP-W2-08)
- Migration of mock components (WP-W2-08)
- E2E tests (Tauri window automation deferred to Week 3 — see ADR follow-up)

## Deliverables

- [ ] `app/package.json`, `app/vite.config.ts`, `app/tsconfig.json`
- [ ] `app/src/main.tsx`, `app/src/App.tsx`, `app/src/styles.css` (imports tokens)
- [ ] `app/index.html` (sets `<html class="dark" lang="en">`, title "Neuron")
- [ ] `app/vitest.config.ts` (jsdom env, setup file pointer)
- [ ] `app/src/test/setup.ts` (imports `@testing-library/jest-dom`)
- [ ] `app/src/App.test.tsx` (smoke test — see "Smoke test contract" below)
- [ ] `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, `src-tauri/build.rs`
- [ ] `src-tauri/src/main.rs` (single `tauri::Builder::default().run(...)` setup)
- [ ] `src-tauri/icons/` (placeholder Tauri icons; can be Tauri default for Week 2)
- [ ] Repo-root `package.json` with `tauri:dev`, `dev`, and `test` scripts
- [ ] Repo-root `pnpm-workspace.yaml`
- [ ] Repo-root `Cargo.toml` (workspace, members `["src-tauri"]`)
- [ ] `.nvmrc` (`20.x`) or `.tool-versions`
- [ ] Update root `.gitignore` with `node_modules/`, `dist/`, `target/`, `src-tauri/target/`, `app/.vite/`, `coverage/`

## Smoke test contract (`app/src/App.test.tsx`)

The smoke test is the WP's automated proof that (a) the React harness renders, (b) the design tokens import correctly, and (c) the dark-first surface token has the expected OKLCH value. It must:

1. Mount `<App />` inside a Testing Library `render` and assert that `screen.getByText('Hello Neuron')` is in the document.
2. Read the computed style of the rendered root and assert that `--surface-bg` (or the equivalent token name from `colors_and_type.css`) resolves to a string containing `oklch(0.135 0.032 258`. A loose `toContain` match is sufficient — full string equality is brittle across browsers/jsdom versions.
3. Run cleanly under `pnpm test --run` (CI mode, no watcher) and exit 0.

If the OKLCH assertion is unreliable under jsdom (jsdom's CSSOM may not parse OKLCH in all versions), fall back to asserting the raw CSS variable string from `getComputedStyle(document.documentElement).getPropertyValue('--surface-bg')`. Document the chosen approach in a code comment so future readers understand why.

## Acceptance criteria

- [ ] `pnpm install` at repo root succeeds with no errors
- [ ] `pnpm tauri dev` opens a desktop window titled "Neuron"
- [ ] The window shows "Hello Neuron" rendered in the brand display font (Geist or Inter fallback)
- [ ] Background color is `oklch(0.135 0.032 258)` (midnight-950) — dark mode default
- [ ] `cargo check --manifest-path src-tauri/Cargo.toml` exits 0
- [ ] `pnpm typecheck` exits 0
- [ ] `pnpm lint` exits 0 (eslint with @typescript-eslint, base config, no rule overrides yet)
- [ ] **`pnpm test --run` exits 0 with at least one passing test (the smoke test)**
- [ ] No frontend mock files (`Neuron Design/app/*`) modified or deleted

## Verification commands

```bash
# from repo root
pnpm install
pnpm typecheck
pnpm lint
pnpm test --run
cargo check --manifest-path src-tauri/Cargo.toml
# manual: pnpm tauri dev → window opens with "Hello Neuron" → close
```

## Notes / risks

- Tauri 2 is the locked target (per Charter). Do NOT use Tauri 1.x patterns (`tauri::Manager`, `Window::emit` signatures changed).
- On Windows, `pnpm tauri dev` requires Microsoft Edge WebView2 runtime (default on Win 10/11).
- Vite dev server port: keep `5173`. If conflict, configure `tauri.conf.json` `build.devPath` accordingly.
- `colors_and_type.css` is imported from the prototype dir for now (transient). WP-W2-08 will move it into `app/src/styles/`.
- Do NOT install React Router yet. Single page in this WP.
- Do NOT install TanStack Query, Tailwind, shadcn yet. Those land in WP-W2-08.
- Pin Tauri to a specific minor version in Cargo.toml (e.g., `tauri = "2.0"`) — track exact version per Charter risk register.
- Vitest version: use the latest 2.x line (matches Vite 5). Pin in `package.json` to avoid silent major bumps.
- jsdom OKLCH parsing has improved in recent versions but is not universal. The smoke test's fallback path (raw `getPropertyValue`) protects against this.
- E2E tests against a live Tauri window (Playwright + WebDriver) are a Week 3 follow-up. Captured here so it does not surprise anyone in WP-08.

## Sub-agent reminders

- Read `PROJECT_CHARTER.md` § "Tech stack" before editing `package.json` / `Cargo.toml`
- Read `AGENTS.md` § "Path conventions" — files belong at the listed paths exactly
- Do NOT add a top-level README.md (the prototype's README in `Neuron Design/` covers the project for now)
- Do NOT change git config, do NOT push to remote, do NOT use `--no-verify` on commit
- The smoke test exists to make the Charter's `pnpm test --run` gate meaningful from WP-01. Do NOT skip it, do NOT mark it `it.skip`, do NOT replace it with a trivial `expect(true).toBe(true)`.
