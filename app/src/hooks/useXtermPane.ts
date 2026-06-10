import { useCallback, useEffect, useRef, type MutableRefObject } from 'react';
import { Terminal as XTerm } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { usePaneLines } from './usePaneLines';
import { useTauriEvent } from './useTauriEvent';
import { useTerminalResize, useTerminalWrite } from './mutations';
import type { PaneLine } from '../lib/bindings';

export interface UseXtermPaneOptions {
  /** xterm font size; Terminal panes use 12, the 9-pane swarm grid 11. */
  fontSize?: number;
  /**
   * `false` for dead PTYs (closed/error/success): skips the
   * `panes:{id}:line` subscription AND the `terminal:resize` IPC —
   * no events will ever fire and the PTY behind the resize is gone.
   */
  live?: boolean;
  /** Exposes xterm's `clear()` to a header button (UI-004). */
  clearRef?: MutableRefObject<(() => void) | null>;
}

/**
 * Everything an xterm-backed pane body shares — extracted from the
 * near-byte-identical `Terminal.tsx::PaneBody` / `SwarmPane.tsx` pair:
 *
 * - xterm mount/teardown keyed on `paneId`
 * - keystrokes → `terminal:write`
 * - ResizeObserver → inline `fit()` + trailing-debounced `terminal:resize`
 * - snapshot hydration (`terminal:lines`, watermarked so line events
 *   don't re-scan the whole scrollback)
 * - live `panes:{id}:line` subscription via `useTauriEvent`
 *
 * Ordering contract: live events that arrive BEFORE the snapshot has
 * hydrated are parked in a buffer and flushed right after it — writing
 * them immediately would render them above older scrollback (the
 * snapshot/live race). After hydration a monotonic seq watermark drops
 * late duplicates without the previous unbounded written-seq Set.
 *
 * Returns the ref to attach to the container `<div>`.
 */
export function useXtermPane(
  paneId: string,
  { fontSize = 12, live = true, clearRef }: UseXtermPaneOptions = {},
): MutableRefObject<HTMLDivElement | null> {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const xtermRef = useRef<XTerm | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  // High-water mark into the cached snapshot array — keeps the
  // snapshot effect O(new tail) instead of O(n) per line event.
  const seenSnapshotLenRef = useRef(0);
  // Highest seq written to xterm; anything at or below is a duplicate.
  const maxWrittenSeqRef = useRef(Number.NEGATIVE_INFINITY);
  const snapshotAppliedRef = useRef(false);
  const pendingLiveRef = useRef<PaneLine[]>([]);
  // `live: false` — the snapshot hydrates xterm once; the live stream
  // is owned by this hook's own subscription below. Letting
  // usePaneLines also subscribe would duplicate the listener and grow
  // an unbounded line array per pane.
  const { data: snapshot, isError: snapshotFailed } = usePaneLines(paneId, {
    live: false,
  });
  const writeMut = useTerminalWrite();
  const resizeMut = useTerminalResize();

  const writeIfNew = useCallback((term: XTerm, line: PaneLine) => {
    if (line.seq <= maxWrittenSeqRef.current) return;
    maxWrittenSeqRef.current = line.seq;
    term.write(line.text + '\r\n');
  }, []);

  // Mount xterm once per pane. The PTY lifecycle is independent of
  // the React render — drop the instance only when the pane id
  // changes, not on every render.
  useEffect(() => {
    if (!containerRef.current) return;
    const term = new XTerm({
      fontFamily: 'var(--font-mono), Menlo, Consolas, monospace',
      fontSize,
      theme: { background: '#0a0a0f', foreground: '#e6e6ea' },
      cursorBlink: true,
      convertEol: true,
      scrollback: 5000,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(containerRef.current);
    try {
      fit.fit();
    } catch {
      // fit() can throw if the container has 0 dimensions during
      // initial mount; ResizeObserver below picks it up shortly.
    }
    const onDataDisp = term.onData((data) => {
      writeMut.mutate({ paneId, data });
    });
    xtermRef.current = term;
    fitRef.current = fit;
    if (clearRef) {
      clearRef.current = () => term.clear();
    }
    return () => {
      onDataDisp.dispose();
      term.dispose();
      xtermRef.current = null;
      fitRef.current = null;
      if (clearRef) clearRef.current = null;
      seenSnapshotLenRef.current = 0;
      maxWrittenSeqRef.current = Number.NEGATIVE_INFINITY;
      snapshotAppliedRef.current = false;
      pendingLiveRef.current = [];
    };
    // pane id is the only useful dep — fontSize/clearRef are stable for
    // a pane's lifetime and the mutations are stable TanStack refs.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [paneId]);

  // Resize observer: refit xterm when the container size changes,
  // then propagate the new cols/rows to the PTY. Window drags fire
  // ResizeObserver every animation frame; `fit()` stays inline so
  // the visual snaps immediately, but the IPC `terminal:resize` is
  // trailing-debounced so the PTY only sees the final size.
  useEffect(() => {
    if (!containerRef.current) return;
    let pendingTimer: ReturnType<typeof setTimeout> | null = null;
    let lastCols = -1;
    let lastRows = -1;
    const obs = new ResizeObserver(() => {
      const fit = fitRef.current;
      const term = xtermRef.current;
      if (!fit || !term) return;
      try {
        fit.fit();
      } catch {
        return;
      }
      // Skip the IPC entirely if the cell grid didn't actually change
      // — happens often when the wrapper resizes by a sub-cell amount.
      if (term.cols === lastCols && term.rows === lastRows) return;
      lastCols = term.cols;
      lastRows = term.rows;
      if (pendingTimer != null) clearTimeout(pendingTimer);
      // Dead panes have no PTY behind them; resize would 404.
      if (!live) return;
      pendingTimer = setTimeout(() => {
        pendingTimer = null;
        const t = xtermRef.current;
        if (!t) return;
        resizeMut.mutate({ paneId, cols: t.cols, rows: t.rows });
      }, 80);
    });
    obs.observe(containerRef.current);
    return () => {
      obs.disconnect();
      if (pendingTimer != null) clearTimeout(pendingTimer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [paneId, live]);

  // Write the snapshot scrollback once it arrives, then incrementally
  // append new tail entries. A failed snapshot query also opens the
  // gate — otherwise live events would buffer forever below.
  useEffect(() => {
    const term = xtermRef.current;
    if (!term) return;
    if (!snapshot && !snapshotFailed) return;
    const lines = snapshot ?? [];
    // Snapshot length shrank — cache was reset (refetch / pane swap);
    // restart from index 0 (the watermark drops re-seen lines).
    if (lines.length < seenSnapshotLenRef.current) {
      seenSnapshotLenRef.current = 0;
    }
    for (let i = seenSnapshotLenRef.current; i < lines.length; i++) {
      writeIfNew(term, lines[i]!);
    }
    seenSnapshotLenRef.current = lines.length;
    if (!snapshotAppliedRef.current) {
      snapshotAppliedRef.current = true;
      const parked = pendingLiveRef.current;
      pendingLiveRef.current = [];
      for (const line of parked) {
        writeIfNew(term, line);
      }
    }
  }, [snapshot, snapshotFailed, writeIfNew]);

  // Live subscription — write each event payload as a single line.
  useTauriEvent<PaneLine>(
    live ? `panes:${paneId}:line` : null,
    (incoming) => {
      const term = xtermRef.current;
      if (!term) return;
      if (!snapshotAppliedRef.current) {
        // Snapshot still in flight — park the event (see contract in
        // the hook docs). Flushed by the snapshot effect.
        pendingLiveRef.current.push(incoming);
        return;
      }
      writeIfNew(term, incoming);
    },
  );

  return containerRef;
}
