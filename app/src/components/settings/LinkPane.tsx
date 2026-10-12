// Pointer pane for sections whose primary surface is a dedicated tab —
// the settings pane is a brief signpost rather than a duplicate of that
// tab's controls.
export function LinkPane({ label, body }: { label: string; body: string }): JSX.Element {
  return (
    <>
      <h2 className="text-h2" style={{ marginTop: 0 }}>
        {label}
      </h2>
      <p className="text-muted">{body}</p>
    </>
  );
}
