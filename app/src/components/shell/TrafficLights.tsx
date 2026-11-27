import { useEffect, useState } from "react";

export function TrafficLights() {
  const isElectron = Boolean(window.octopusTransport);
  const [customControls, setCustomControls] = useState(false);
  const [chromeHidden, setChromeHidden] = useState(() => document.documentElement.dataset.chromeRevealed === "false");
  useEffect(() => {
    const transport = window.octopusTransport;
    if (!transport) return;
    let cancelled = false;
    void transport.windowState().then((state) => { if (!cancelled) setCustomControls(state.customWindowControls); });
    const unsubscribe = transport.onWindowStateChanged((state) => setCustomControls(state.customWindowControls));
    return () => { cancelled = true; unsubscribe(); };
  }, []);
  useEffect(() => {
    const root = document.documentElement;
    const sync = () => setChromeHidden(root.dataset.chromeRevealed === "false");
    sync();
    if (typeof MutationObserver === "undefined") return;
    const observer = new MutationObserver(sync);
    observer.observe(root, { attributes: true, attributeFilter: ["data-chrome-revealed"] });
    return () => observer.disconnect();
  }, []);
  if (!isElectron || !customControls) return null;
  return (
    <div className={`app-traffic-lights no-drag${chromeHidden ? " is-chrome-hidden" : ""}`} inert={chromeHidden || undefined}>
      <button
        aria-label="Close window"
        className="app-traffic-light"
        onClick={() => window.octopusTransport?.windowControl("close")}
        type="button"
      >
        <svg aria-hidden="true" viewBox="0 0 8 8">
          <path d="M1.6 1.6l4.8 4.8M6.4 1.6L1.6 6.4" fill="none" stroke="currentColor" strokeLinecap="round" strokeWidth="1.1" />
        </svg>
      </button>
      <button
        aria-label="Minimize window"
        className="app-traffic-light"
        onClick={() => window.octopusTransport?.windowControl("minimize")}
        type="button"
      >
        <svg aria-hidden="true" viewBox="0 0 8 8">
          <path d="M1.3 4h5.4" fill="none" stroke="currentColor" strokeLinecap="round" strokeWidth="1.1" />
        </svg>
      </button>
      <button
        aria-label="Zoom window"
        className="app-traffic-light"
        onClick={() => window.octopusTransport?.windowControl("zoom")}
        type="button"
      >
        <svg aria-hidden="true" viewBox="0 0 8 8">
          <path d="M1.4 6.6V3.7l2.9 2.9Zm5.2-5.2v2.9L3.7 1.4Z" fill="currentColor" />
        </svg>
      </button>
    </div>
  );
}
