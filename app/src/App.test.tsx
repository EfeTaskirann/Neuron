import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { App } from "./App";
import "./styles.css";

// WP-W2-01 smoke test contract.
//
// (a) Renders the placeholder surface. If this fails, the React +
//     Vite + jsdom harness is broken before any feature work begins.
// (b) Asserts the dark-mode background token (`--background`) resolves
//     to the midnight-950 OKLCH value `oklch(0.135 0.032 258)`. The
//     design system in `Neuron Design/colors_and_type.css` exposes the
//     surface as `--background` (semantic), not `--surface-bg`. The WP
//     spec calls the token "--surface-bg" generically; we match the
//     real semantic name and document it here so future readers don't
//     get confused.
// (c) Falls back to the raw CSS variable string when jsdom's CSSOM
//     refuses to compute custom-property OKLCH values. jsdom 25 +
//     Vitest 2 still does not always evaluate `var(--token)` inside
//     `getComputedStyle(...).backgroundColor` reliably; the loose
//     `toContain('oklch(0.135 0.032 258')` match keeps the assertion
//     stable across jsdom minor versions.

describe("App smoke", () => {
  it("renders the Hello Neuron placeholder", () => {
    render(<App />);
    expect(screen.getByText("Hello Neuron")).toBeInTheDocument();
  });

  it("resolves --background to the midnight-950 OKLCH token", () => {
    render(<App />);
    // The dark class is set on <html> by index.html; in jsdom, the
    // initial document root is `<html>` without that attribute, so we
    // re-add it explicitly. This mirrors the production runtime.
    document.documentElement.classList.add("dark");

    const root = document.documentElement;
    const raw = getComputedStyle(root)
      .getPropertyValue("--background")
      .trim();

    // Loose match: any whitespace style that resolves to midnight-950
    // counts. Browsers and jsdom can normalise spacing.
    expect(raw).toContain("oklch(0.135 0.032 258");
  });
});
