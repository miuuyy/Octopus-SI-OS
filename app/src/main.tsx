import React from "react";
import ReactDOM from "react-dom/client";
import { WorkerPoolContextProvider, useWorkerPool } from "@pierre/diffs/react";
import PierreDiffWorker from "@pierre/diffs/worker/worker.js?worker";
import { App } from "./App";
import { RendererFailureBoundary } from "./RendererFailureBoundary";
import { applyAppearance, useAppearance } from "./appearance";
import { octopusDiffThemeName, registerOctopusDiffThemes } from "./components/settings/octopusDiffThemes";
import "./foundation.css";
import "./owned-utilities.css";
import "./octopus-owned-components.css";
import "katex/dist/katex.min.css";
import "./styles.css";
import "./theme.css";
import "./shell.css";
import "@xterm/xterm/css/xterm.css";

applyAppearance();

const pierreThemeRegistrationKey = Symbol.for("octopus.diff-themes.registered");
const pierreThemeRegistry = globalThis as typeof globalThis & { [pierreThemeRegistrationKey]?: boolean };
if (!pierreThemeRegistry[pierreThemeRegistrationKey]) {
  registerOctopusDiffThemes();
  pierreThemeRegistry[pierreThemeRegistrationKey] = true;
}

const pierreWorkerPoolOptions = {
  workerFactory: () => new PierreDiffWorker(),
};

const windowTransport = window.octopusTransport;
if (windowTransport) {
  const applyWindowState = (state: { opaqueSurface: boolean; resizing: boolean }) => {
    document.documentElement.classList.toggle("electron-opaque", state.opaqueSurface);
    document.documentElement.classList.toggle("electron-window-resizing", state.resizing);
  };
  void windowTransport.windowState().then(applyWindowState);
  windowTransport.onWindowStateChanged(applyWindowState);
} else {
  document.documentElement.classList.add("electron-opaque");
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <RendererFailureBoundary>
      <PierreDiffWorkerProvider>
        <App />
      </PierreDiffWorkerProvider>
    </RendererFailureBoundary>
  </React.StrictMode>,
);

function PierreDiffWorkerProvider({ children }: { children: React.ReactNode }) {
  const appearance = useAppearance();
  const theme = React.useMemo(() => ({
    dark: octopusDiffThemeName("dark", appearance.darkCodeTheme),
    light: octopusDiffThemeName("light", appearance.lightCodeTheme),
  }), [appearance.darkCodeTheme, appearance.lightCodeTheme]);
  const highlighterOptions = React.useMemo(() => ({ theme }), [theme]);
  return (
    <WorkerPoolContextProvider highlighterOptions={highlighterOptions} poolOptions={pierreWorkerPoolOptions}>
      <PierreDiffWorkerThemeSync theme={theme} />
      {children}
    </WorkerPoolContextProvider>
  );
}

function PierreDiffWorkerThemeSync({ theme }: { theme: { dark: string; light: string } }) {
  const pool = useWorkerPool();
  const [failure, setFailure] = React.useState<Error | null>(null);
  React.useEffect(() => {
    let active = true;
    setFailure(null);
    if (!pool) {
      setFailure(new Error("Pierre worker pool is unavailable."));
      return () => { active = false; };
    }
    void pool.setRenderOptions({ theme }).catch((error) => {
      if (active) setFailure(error instanceof Error ? error : new Error(String(error)));
    });
    return () => { active = false; };
  }, [pool, theme]);
  if (failure) throw failure;
  return null;
}
