// Ports `Neuron Design/app/inspector.jsx::RunInspector`. Hardcoded
// SPANS → useRun(runId). Span field renames (t0/dur/attrs/running
// → t0Ms/durationMs/attrsJson/isRunning); attrs becomes a parsed
// object derived from the JSON-string column at render time.
import { useMemo, useState } from 'react';
import { NIcon } from '../components/icons';
import { useRun } from '../hooks/useRun';
import type { Span } from '../lib/bindings';

interface RunInspectorProps {
  runId: string | null;
  onClose?: () => void;
}

export function RunInspector({ runId, onClose }: RunInspectorProps): JSX.Element {
  // All hooks live above any early returns — React's Rules of
  // Hooks demand a stable call order across renders. Conditional
  // *content* below is fine; conditional *hook calls* are not.
  const { data, isLoading, isError, error } = useRun(runId);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<'Spans' | 'Logs' | 'Output'>('Spans');

  // Total duration drives the timeline X axis. While spans are
  // still streaming `run.dur` may be null; fall back to the max
  // (t0 + dur) of seen spans so the bars stay sensibly scaled.
  // Computed defensively so the hook works even before `data`
  // arrives.
  const total = useMemo(() => {
    if (!data) return 1;
    if (data.run.dur) return data.run.dur;
    const lastEdge = data.spans.reduce(
      (max, s) => Math.max(max, s.t0Ms + (s.durationMs ?? 0)),
      0,
    );
    return Math.max(lastEdge, 1);
  }, [data]);

  if (!runId) {
    return (
      <div className="inspector inspector-empty">
        <p className="text-muted">Select a run to inspect its trace.</p>
      </div>
    );
  }
  if (isLoading) {
    return <div className="inspector inspector-loading">Loading run…</div>;
  }
  if (isError) {
    throw error instanceof Error ? error : new Error(String(error));
  }
  if (!data) return <></>;

  const { run, spans } = data;
  const selectedSpan =
    spans.find((s) => s.id === (selectedId ?? spans[0]?.id)) ?? spans[0] ?? null;

  return (
    <div className="inspector">
      <div className="inspector-head">
        <div className="ins-head-l">
          <span className="ins-overline">Run · {run.id}</span>
          <h3 className="ins-title">{run.workflow}</h3>
        </div>
        <div className="ins-head-r">
          <span className={`pill st-${run.status === 'success' ? 'ok' : run.status === 'running' ? 'running' : 'error'}`}>
            {run.status === 'running' && <span className="pulse-dot" />}
            {run.status}
          </span>
          <span className="pill st-outline">{run.tokens.toLocaleString()} tokens</span>
          <span className="pill st-outline">${run.cost.toFixed(4)}</span>
          {onClose && (
            <button className="icon-btn" onClick={onClose} title="Close">
              <NIcon name="close" size={14} />
            </button>
          )}
        </div>
      </div>

      <nav className="inspector-tabs">
        {(['Spans', 'Logs', 'Output'] as const).map((tab) => (
          <button
            key={tab}
            className={`ins-tab${activeTab === tab ? ' active' : ''}`}
            onClick={() => setActiveTab(tab)}
          >
            {tab}
          </button>
        ))}
      </nav>

      <div className="inspector-body">
        <div className="span-axis">
          <span>Span</span>
          <span className="span-axis-marks">
            <span>0ms</span>
            <span>{formatDur(total / 2)}</span>
            <span>{formatDur(total)}</span>
          </span>
        </div>
        {spans.length === 0 ? (
          <p className="text-muted" style={{ padding: '12px 16px' }}>
            No spans yet — they'll appear as the run progresses.
          </p>
        ) : (
          spans.map((span) => (
            <SpanRow
              key={span.id}
              span={span}
              total={total}
              selected={selectedSpan?.id === span.id}
              onSelect={setSelectedId}
            />
          ))
        )}
        {selectedSpan && <SelectedSpanSheet span={selectedSpan} />}
      </div>
    </div>
  );
}

interface SpanRowProps {
  span: Span;
  total: number;
  selected: boolean;
  onSelect: (id: string) => void;
}

function SpanRow({ span, total, selected, onSelect }: SpanRowProps): JSX.Element {
  const left = (span.t0Ms / total) * 100;
  const dur = span.durationMs ?? Math.max(0, total - span.t0Ms);
  const width = (dur / total) * 100;
  const barClass = `span-bar kind-${span.type}${span.isRunning ? ' running wf-shimmer' : ''}`;
  const rowClass = `span-row${selected ? ' selected' : ''}`;
  return (
    <div className={rowClass} onClick={() => onSelect(span.id)}>
      <div className="span-label" style={{ paddingLeft: 10 + span.indent * 16 }}>
        <span className={`span-dot kind-${span.type}`} />
        <span className="span-name">{span.name}</span>
        <span className="span-glyph">{span.indent > 0 ? '└' : ''}</span>
      </div>
      <div className="span-track">
        <div className={barClass} style={{ left: `${left}%`, width: `${width}%` }} />
        <span className="span-dur">{formatDur(span.durationMs)}</span>
      </div>
    </div>
  );
}

function SelectedSpanSheet({ span }: { span: Span }): JSX.Element {
  // attrs_json is a wire-format JSON string; parse defensively so
  // a malformed payload doesn't crash the inspector.
  const attrs = useMemo<Record<string, unknown>>(() => {
    if (!span.attrsJson) return {};
    try {
      const v = JSON.parse(span.attrsJson);
      return v && typeof v === 'object' ? (v as Record<string, unknown>) : {};
    } catch {
      return {};
    }
  }, [span.attrsJson]);
  const attrEntries = Object.entries(attrs);
  return (
    <div className="span-sheet">
      <div className="span-sheet-head">
        <span className={`span-dot kind-${span.type}`} />
        <span className="span-sheet-name">{span.name}</span>
        <span className="span-sheet-meta">· {formatDur(span.durationMs)}</span>
        {span.isRunning && (
          <span className="pill st-running">
            <span className="pulse-dot" />
            running
          </span>
        )}
      </div>

      {attrEntries.length > 0 && (
        <div className="span-attrs">
          {attrEntries.map(([k, v]) => (
            <div key={k} className="span-attr-row">
              <span className="span-attr-k">{k}</span>
              <span className="span-attr-v">{formatAttrValue(v)}</span>
            </div>
          ))}
        </div>
      )}

      {span.type === 'llm' && (span.prompt || span.response) && (
        <div className="span-llm">
          {span.prompt && (
            <div className="span-llm-block">
              <div className="span-llm-label">Prompt</div>
              <pre className="span-llm-snippet mute">{span.prompt}</pre>
            </div>
          )}
          {span.response && (
            <div className="span-llm-block">
              <div className="span-llm-label">Response</div>
              <pre className="span-llm-snippet">
                {span.response}
                {span.isRunning && <span className="ins-stream-cursor" />}
              </pre>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function formatDur(ms: number | null): string {
  if (ms == null) return '—';
  if (ms < 1000) return `${Math.round(ms)}ms`;
  return `${(ms / 1000).toFixed(2)}s`;
}

function formatAttrValue(v: unknown): string {
  if (v === true) return 'true';
  if (v === false) return 'false';
  if (v == null) return '—';
  if (typeof v === 'number') {
    if (Number.isInteger(v)) return v.toLocaleString();
    return v.toString();
  }
  return String(v);
}
