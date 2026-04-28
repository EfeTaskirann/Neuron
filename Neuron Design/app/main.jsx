/* global React, ReactDOM, AppShell, WorkflowCanvas, RunInspector,
            AgentsRoute, RunsRoute, MCPRoute, SettingsRoute,
            useTweaks, TweaksPanel, TweakSection, TweakRadio, TweakToggle, TweakSelect */
const { useState, useEffect } = React;

const TWEAK_DEFAULTS = /*EDITMODE-BEGIN*/{
  "accent": "violet",
  "density": "comfortable",
  "motion": "full",
  "showInspector": true
}/*EDITMODE-END*/;

const ACCENT_HUES = {
  violet: { 50:"oklch(0.977 0.014 308)", 100:"oklch(0.946 0.029 305)", 200:"oklch(0.894 0.060 303)",
            300:"oklch(0.823 0.103 301)", 400:"oklch(0.737 0.159 299)", 500:"oklch(0.643 0.214 298)",
            600:"oklch(0.555 0.226 297)", 700:"oklch(0.470 0.207 296)" },
  azure:  { 50:"oklch(0.975 0.014 232)", 100:"oklch(0.940 0.030 232)", 200:"oklch(0.880 0.065 232)",
            300:"oklch(0.795 0.115 232)", 400:"oklch(0.715 0.145 232)", 500:"oklch(0.625 0.180 240)",
            600:"oklch(0.530 0.180 245)", 700:"oklch(0.435 0.155 247)" },
  ember:  { 50:"oklch(0.975 0.014 35)",  100:"oklch(0.945 0.030 33)",  200:"oklch(0.890 0.065 32)",
            300:"oklch(0.815 0.115 33)", 400:"oklch(0.735 0.155 35)", 500:"oklch(0.640 0.190 36)",
            600:"oklch(0.550 0.195 36)", 700:"oklch(0.460 0.170 35)" },
  jade:   { 50:"oklch(0.975 0.018 165)", 100:"oklch(0.945 0.040 165)", 200:"oklch(0.880 0.080 165)",
            300:"oklch(0.800 0.120 165)", 400:"oklch(0.715 0.150 162)", 500:"oklch(0.620 0.155 160)",
            600:"oklch(0.520 0.135 158)", 700:"oklch(0.420 0.110 158)" },
};

function applyAccent(hue) {
  const ramp = ACCENT_HUES[hue] || ACCENT_HUES.violet;
  const r = document.documentElement;
  Object.entries(ramp).forEach(([k, v]) => r.style.setProperty(`--neuron-violet-${k}`, v));
}

function App() {
  const [tweaks, setTweak] = useTweaks(TWEAK_DEFAULTS);
  const [route, setRoute] = useState("canvas");
  const [showInspector, setShowInspector] = useState(true);

  useEffect(() => { applyAccent(tweaks.accent); }, [tweaks.accent]);
  useEffect(() => {
    document.documentElement.dataset.density = tweaks.density;
    document.documentElement.dataset.motion = tweaks.motion;
  }, [tweaks.density, tweaks.motion]);

  const inspectorVisible = route === "canvas" && showInspector && tweaks.showInspector;
  const routeEl = {
    canvas:   <WorkflowCanvas onSelectRun={() => setShowInspector(true)} />,
    terminal: <TerminalRoute/>,
    agents:   <AgentsRoute/>,
    runs:     <RunsRoute/>,
    mcp:      <MCPRoute/>,
    settings: <SettingsRoute/>,
  }[route];

  return (
    <>
      <AppShell route={route} setRoute={setRoute}
                inspector={inspectorVisible ? <RunInspector onClose={() => setShowInspector(false)}/> : null}>
        {routeEl}
      </AppShell>

      <TweaksPanel title="Tweaks">
        <TweakSection title="Color">
          <TweakRadio label="Accent" value={tweaks.accent}
            onChange={v => setTweak('accent', v)}
            options={[
              { value:"violet", label:"Violet" },
              { value:"azure",  label:"Azure" },
              { value:"ember",  label:"Ember" },
              { value:"jade",   label:"Jade" },
            ]}/>
        </TweakSection>
        <TweakSection title="Layout">
          <TweakRadio label="Density" value={tweaks.density}
            onChange={v => setTweak('density', v)}
            options={[
              { value:"comfortable", label:"Comfortable" },
              { value:"compact",     label:"Compact" },
            ]}/>
          <TweakToggle label="Inspector" checked={tweaks.showInspector}
            onChange={v => setTweak('showInspector', v)}/>
        </TweakSection>
        <TweakSection title="Motion">
          <TweakRadio label="Animations" value={tweaks.motion}
            onChange={v => setTweak('motion', v)}
            options={[
              { value:"full",    label:"Full" },
              { value:"reduced", label:"Reduced" },
              { value:"off",     label:"Off" },
            ]}/>
        </TweakSection>
      </TweaksPanel>
    </>
  );
}

ReactDOM.createRoot(document.getElementById("root")).render(<App/>);
