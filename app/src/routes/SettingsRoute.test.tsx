import { describe, expect, it, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';

// T1-01 render-smoke for the `settings` tab, which had none. The route
// has no query deps; its only stateful child is the Appearance pane via
// `useAppearance`. Mock the hook to a fixed state (keep ACCENT_SWATCHES
// real) so the test asserts structure without touching localStorage or
// mutating the <html> element.
vi.mock('../hooks/useAppearance', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../hooks/useAppearance')>();
  return {
    ...actual,
    useAppearance: vi.fn(() => ({
      theme: 'dark' as const,
      accent: '#a874d6',
      density: 'comfortable' as const,
      motion: 'full' as const,
      setTheme: vi.fn(),
      setAccent: vi.fn(),
      setDensity: vi.fn(),
      setMotion: vi.fn(),
    })),
  };
});

import { SettingsRoute } from './SettingsRoute';

describe('SettingsRoute', () => {
  it('renders the section nav and defaults to the Appearance pane', () => {
    render(<SettingsRoute />);
    // Section nav buttons.
    expect(screen.getByRole('button', { name: /account/i })).toBeInTheDocument();
    expect(
      screen.getByRole('button', { name: /appearance/i }),
    ).toBeInTheDocument();
    // Appearance pane is active by default — its labelled radiogroups render.
    expect(screen.getByRole('radiogroup', { name: /theme/i })).toBeInTheDocument();
    expect(
      screen.getByRole('radiogroup', { name: /accent color/i }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole('radiogroup', { name: /density/i }),
    ).toBeInTheDocument();
    expect(screen.getByRole('radiogroup', { name: /motion/i })).toBeInTheDocument();
  });

  it('switches to a placeholder pane when a non-appearance section is picked', () => {
    render(<SettingsRoute />);
    fireEvent.click(screen.getByRole('button', { name: /workflows/i }));
    expect(screen.getByText(/settings for this section/i)).toBeInTheDocument();
    // The appearance radiogroups are gone once another section is active.
    expect(
      screen.queryByRole('radiogroup', { name: /theme/i }),
    ).not.toBeInTheDocument();
  });
});
