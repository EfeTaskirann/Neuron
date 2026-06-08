import { memo, useEffect, useRef } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { Terminal as XTerm } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { usePaneLines } from '../hooks/usePaneLines';
import {
  useTerminalResize,
  useTerminalWrite,
} from '../hooks/mutations';
import type { PaneLine } from '../lib/bindings';

interface Props {
  paneId: string;
  agentId: string;
}

function SwarmPaneImpl({ paneId, agentId }: Props): JSX.Element {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const xtermRef = useRef<XTerm | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const writtenSeqsRef = useRef<Set<number>>(new Set());
  const seenSnapshotLenRef = useRef(0);
  // `live: false` — the snapshot hydrates xterm once on mount; the live
  // `panes:{id}:line` stream is handled by this component's own listener
  // below, so we don't want usePaneLines to open a second subscription
  // (and grow an unbounded line array) for the same channel.
  const { data: snapshot } = usePaneLines(paneId, { live: false });
  const writeMut = useTerminalWrite();
  const resizeMut = useTerminalResize();

  useEffect(() => {
    if (!containerRef.current) return;
    const writtenSeqs = writtenSeqsRef.current;
    const term = new XTerm({
      fontFamily: 'var(--font-mono), Menlo, Consolas, monospace',
      fontSize: 11,
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
      /* deferred to ResizeObserver */
    }
    const onDataDisp = term.onData((data) => {
      writeMut.mutate({ paneId, data });
    });
    xtermRef.current = term;
    fitRef.current = fit;
    return () => {
      onDataDisp.dispose();
      term.dispose();
      xtermRef.current = null;
      fitRef.current = null;
      writtenSeqs.clear();
      seenSnapshotLenRef.current = 0;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [paneId]);

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
      if (term.cols === lastCols && term.rows === lastRows) return;
      lastCols = term.cols;
      lastRows = term.rows;
      if (pendingTimer != null) clearTimeout(pendingTimer);
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
  }, [paneId]);

  useEffect(() => {
    const term = xtermRef.current;
    if (!term || !snapshot) return;
    if (snapshot.length < seenSnapshotLenRef.current) {
      seenSnapshotLenRef.current = 0;
    }
    for (let i = seenSnapshotLenRef.current; i < snapshot.length; i++) {
      const line = snapshot[i]!;
      if (writtenSeqsRef.current.has(line.seq)) continue;
      writtenSeqsRef.current.add(line.seq);
      term.write(line.text + '\r\n');
    }
    seenSnapshotLenRef.current = snapshot.length;
  }, [snapshot]);

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let cancelled = false;
    listen<PaneLine>(`panes:${paneId}:line`, (event) => {
      const term = xtermRef.current;
      if (!term) return;
      const incoming = event.payload;
      if (writtenSeqsRef.current.has(incoming.seq)) return;
      writtenSeqsRef.current.add(incoming.seq);
      term.write(incoming.text + '\r\n');
    })
      .then((fn) => {
        if (cancelled) {
          fn();
        } else {
          unlisten = fn;
        }
      })
      .catch((err) => {
        console.warn(`[SwarmPane:${agentId}] subscribe failed`, err);
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [paneId, agentId]);

  return <div className="swarm-term-xterm" ref={containerRef} />;
}

// Memoized: TerminalSwarmRoute re-renders on its routing-edge (~1.5 s)
// and lifecycle (5 s) timers. `paneId`/`agentId` are stable for a pane's
// lifetime, so memo keeps those parent ticks from reconciling all 9
// xterm panes for nothing.
export const SwarmPane = memo(SwarmPaneImpl);
