import { useState } from 'react';
import { NIcon } from '../icons';
import { useMailbox } from '../../hooks/useMailbox';
import type { MailboxEntry } from '../../lib/bindings';

// Cross-pane event log. Renders as a slim header strip above the
// pane grid — keeps the visual hierarchy: messages first, then the
// running panes. Empty state is hidden (no row at all) so the
// pane grid can claim the full vertical space when there's nothing
// to surface.
export function MailboxPanel(): JSX.Element | null {
  const { data: entries = [] } = useMailbox();
  const [expanded, setExpanded] = useState(false);
  if (entries.length === 0) return null;
  const visible = expanded ? entries : entries.slice(0, 3);
  return (
    <div className="mailbox-panel" aria-label="Mailbox">
      <div className="mailbox-head">
        <NIcon name="activity" size={12} />
        <span className="mailbox-title">Mailbox · {entries.length}</span>
        {entries.length > 3 && (
          <button className="mailbox-toggle" onClick={() => setExpanded((v) => !v)}>
            {expanded ? 'Collapse' : 'Show all'}
          </button>
        )}
      </div>
      <ul className="mailbox-list">
        {visible.map((entry) => (
          <MailboxRow key={entry.id} entry={entry} />
        ))}
      </ul>
    </div>
  );
}

function MailboxRow({ entry }: { entry: MailboxEntry }): JSX.Element {
  return (
    <li className="mailbox-row">
      <span className="mailbox-ts">{formatRelative(entry.ts)}</span>
      <code className="mailbox-from">{entry.from}</code>
      <NIcon name="arrowR" size={10} />
      <code className="mailbox-to">{entry.to}</code>
      <span className="mailbox-type">{entry.type}</span>
      <span className="mailbox-summary">{entry.summary}</span>
    </li>
  );
}

function formatRelative(ts: number): string {
  const delta = Math.max(0, Math.floor(Date.now() / 1000) - ts);
  if (delta < 60) return `${delta}s`;
  if (delta < 3600) return `${Math.floor(delta / 60)}m`;
  if (delta < 86400) return `${Math.floor(delta / 3600)}h`;
  return `${Math.floor(delta / 86400)}d`;
}
