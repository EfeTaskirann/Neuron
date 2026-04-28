/* global React, NeuronUI */
const { useState } = React;
const { Icon, Button } = NeuronUI;

const NAV = ["General", "Models", "Keys & Secrets", "MCP Servers", "Telemetry", "Appearance", "Keyboard", "About"];

const Toggle = ({ on, onChange }) => (
  <button onClick={() => onChange(!on)} style={{
    width: 32, height: 18, borderRadius: 9999, border: "none", padding: 0, cursor: "pointer",
    background: on ? "var(--neuron-violet-500)" : "var(--neuron-midnight-700)",
    position: "relative", transition: "background 160ms var(--ease-out)",
  }}>
    <span style={{
      position: "absolute", top: 2, left: on ? 16 : 2, width: 14, height: 14,
      borderRadius: 9999, background: "#fff", transition: "left 160ms var(--ease-out)",
    }} />
  </button>
);

const RadioRow = ({ value, active, onClick, children }) => (
  <button onClick={() => onClick(value)} style={{
    height: 28, padding: "0 12px", borderRadius: 6, fontSize: 12, fontWeight: 500,
    background: active ? "var(--card)" : "transparent",
    color: active ? "var(--foreground)" : "var(--muted-foreground)",
    border: active ? "1px solid var(--border)" : "1px solid transparent",
    cursor: "pointer", fontFamily: "var(--font-sans)",
    boxShadow: active ? "var(--shadow-xs)" : "none",
    display: "flex", alignItems: "center", gap: 6,
  }}>{children}</button>
);

const Field = ({ label, helper, children }) => (
  <div style={{ display: "grid", gridTemplateColumns: "240px 1fr", gap: 24, padding: "16px 0", borderBottom: "1px solid var(--border)", alignItems: "center" }}>
    <div>
      <div style={{ fontSize: 13, fontWeight: 500 }}>{label}</div>
      {helper && <div style={{ fontSize: 12, color: "var(--muted-foreground)", marginTop: 2 }}>{helper}</div>}
    </div>
    <div>{children}</div>
  </div>
);

const Settings = ({ theme, setTheme }) => {
  const [section, setSection] = useState("Appearance");
  const [density, setDensity] = useState("comfortable");
  const [telemetry, setTelemetry] = useState(true);

  return (
    <div style={{ display: "grid", gridTemplateColumns: "220px 1fr", height: "100%", overflow: "hidden" }}>
      {/* Left nav */}
      <nav style={{ borderRight: "1px solid var(--border)", padding: "16px 8px", overflow: "auto" }}>
        <div style={{ fontSize: 11, fontWeight: 600, letterSpacing: "0.08em", textTransform: "uppercase", color: "var(--muted-foreground)", padding: "0 12px 8px" }}>Settings</div>
        {NAV.map(item => (
          <button key={item} onClick={() => setSection(item)} style={{
            display: "flex", alignItems: "center", width: "100%", height: 32,
            padding: "0 12px", border: "none", cursor: "pointer", fontFamily: "var(--font-sans)",
            background: section === item ? "var(--accent)" : "transparent",
            color: section === item ? "var(--accent-foreground)" : "var(--foreground)",
            borderRadius: 6, fontSize: 13, fontWeight: 500, textAlign: "left", marginBottom: 2,
            position: "relative",
          }}>
            {section === item && <span style={{ position: "absolute", left: -8, top: 6, bottom: 6, width: 2, borderRadius: 2, background: "var(--neuron-violet-500)" }} />}
            {item}
          </button>
        ))}
      </nav>
      {/* Form */}
      <div style={{ padding: "32px 48px", overflow: "auto" }}>
        <div style={{ maxWidth: 640 }}>
          <div style={{ fontSize: 24, fontWeight: 600, letterSpacing: "-0.01em" }}>{section}</div>
          <div style={{ fontSize: 13, color: "var(--muted-foreground)", marginTop: 4 }}>
            {section === "Appearance" ? "Theme, density, and accent color." : "Configure Neuron to your taste."}
          </div>
          <div style={{ height: 1, background: "var(--border)", margin: "24px 0 8px" }} />

          {section === "Appearance" ? (
            <>
              <Field label="Theme" helper="Follows your OS by default.">
                <div style={{ display: "inline-flex", gap: 4, padding: 3, background: "var(--muted)", borderRadius: 8 }}>
                  <RadioRow value="light" active={theme === "light"} onClick={setTheme}><Icon name="sun" size={12} />Light</RadioRow>
                  <RadioRow value="dark" active={theme === "dark"} onClick={setTheme}><Icon name="moon" size={12} />Dark</RadioRow>
                  <RadioRow value="system" active={theme === "system"} onClick={setTheme}>System</RadioRow>
                </div>
              </Field>
              <Field label="Density" helper="Tighter rows on dense screens.">
                <div style={{ display: "inline-flex", gap: 4, padding: 3, background: "var(--muted)", borderRadius: 8 }}>
                  <RadioRow value="comfortable" active={density === "comfortable"} onClick={setDensity}>Comfortable</RadioRow>
                  <RadioRow value="compact" active={density === "compact"} onClick={setDensity}>Compact</RadioRow>
                </div>
              </Field>
              <Field label="Accent hue" helper="Slide to retune the violet primary.">
                <input type="range" min="280" max="320" defaultValue="298" style={{ width: 240, accentColor: "var(--neuron-violet-500)" }} />
              </Field>
              <Field label="Reduced motion" helper="Disable spring animations system-wide.">
                <Toggle on={false} onChange={() => {}} />
              </Field>
            </>
          ) : section === "General" ? (
            <>
              <Field label="Workspace name" helper="Shown at top of the sidebar.">
                <input defaultValue="Personal" style={{ height: 32, padding: "0 12px", borderRadius: 8, background: "var(--input)", border: "1px solid var(--border)", color: "var(--foreground)", fontSize: 13, fontFamily: "var(--font-sans)", width: 240, outline: "none" }} />
              </Field>
              <Field label="Language">
                <select style={{ height: 32, padding: "0 8px", borderRadius: 8, background: "var(--input)", border: "1px solid var(--border)", color: "var(--foreground)", fontSize: 13, fontFamily: "var(--font-sans)" }}>
                  <option>English</option><option>Türkçe</option>
                </select>
              </Field>
              <Field label="Telemetry" helper="Send anonymous traces to improve Neuron.">
                <Toggle on={telemetry} onChange={setTelemetry} />
              </Field>
            </>
          ) : (
            <div style={{ display: "flex", flexDirection: "column", alignItems: "center", padding: "60px 0", color: "var(--muted-foreground)", gap: 12 }}>
              <Icon name="settings" size={36} color="var(--neuron-violet-400)" />
              <div style={{ fontSize: 16, fontWeight: 600, color: "var(--foreground)" }}>Coming soon</div>
              <div style={{ fontSize: 13 }}>{section} settings will land in week 2.</div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
};

window.NeuronSettings = Settings;
