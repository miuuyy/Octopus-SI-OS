import { useEffect, useRef, useState } from "react";
import type { WorkerEnvironmentProjection, WorkerEnvironmentSurfaceProjection } from "../../data/topology";
import { OcButton, OcSelect } from "../../design-system/components";
import { useSetClientPreferences } from "../../clientPreferences";
import { dispatchClientCommand } from "../../clientActions";
import { Glyph, type GlyphName } from "../IconGlyphs";
import { ShellTooltip } from "../shell/ShellTooltip";
import { LiveSurfaceEmpty, ManualRoomControlWarning, dataText } from "./RunEnvironmentSurfacePrimitives";
import type { DesktopApplicationPresentation, DesktopEvidence, DesktopSurfaceRequest } from "./desktopSurfaceModel";
import { desktopApplicationPresentations, desktopEvidenceRevision } from "./desktopSurfaceModel";
import "./desktopSurface.css";

export function DesktopSurface({ active = true, environment, surface }: {
  active?: boolean; environment: WorkerEnvironmentProjection; surface?: WorkerEnvironmentSurfaceProjection;
}) {
  const setPreferences = useSetClientPreferences();
  const [evidence, setEvidence] = useState<DesktopEvidence | null>(null);
  const [error, setError] = useState("");
  const [selected, setSelected] = useState("");
  const [pendingControl, setPendingControl] = useState<DesktopSurfaceRequest["operation"] | null>(null);
  const [warning, setWarning] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const generation = useRef(0), requestGeneration = useRef(0), viewId = useRef("");
  const controlPending = useRef<{ operation: DesktopSurfaceRequest["operation"] } | null>(null);
  const identity = `${environment.id}\0${environment.runId}\0${surface?.id}\0${surface?.viewerUrl}`;
  const activeIdentity = useRef(identity); activeIdentity.current = identity;
  const busy = pendingControl !== null;
  let presentations: DesktopApplicationPresentation[] = [], inventoryError = "";
  try { presentations = desktopApplicationPresentations(surface?.data ?? {}).filter(value => value.worker_environment_id === environment.id && value.run_id === environment.runId && value.completion_status === null && value.phase !== "ended"); }
  catch (failure) { inventoryError = failure instanceof Error ? failure.message : String(failure); }
  const application = presentations.find(value => value.source_id === selected) ?? presentations[0];

  useEffect(() => {
    setEvidence(null); setError(""); setSelected(""); setWarning(false); setPendingControl(null); controlPending.current = null;
  }, [identity]);
  useEffect(() => {
    const current = ++generation.current, attachment = crypto.randomUUID(); viewId.current = attachment;
    let timer: ReturnType<typeof setTimeout> | undefined, revision = "";
    const transport = window.octopusTransport?.desktopSurfaceRequest;
    if (!active || !surface || surface.mode === "fixture" || !transport) return;
    const target = { workerEnvironmentId: environment.id, surfaceId: surface.id, runId: environment.runId, viewId: attachment };
    async function read() {
      if (generation.current !== current) return;
      if (controlPending.current) { timer = setTimeout(() => void read(), 500); return; }
      const readGeneration = ++requestGeneration.current;
      try {
        const result = await transport!({ ...target, operation: "view" });
        if (generation.current !== current || requestGeneration.current !== readGeneration) return;
        if (!("evidence" in result)) throw new Error("Desktop status returned no observation evidence");
        const nextRevision = desktopEvidenceRevision(result.evidence);
        if (revision !== nextRevision) { setEvidence(result.evidence); revision = nextRevision; }
        setError(""); timer = setTimeout(() => void read(), 500);
      } catch (failure) {
        if (generation.current === current && requestGeneration.current === readGeneration) { setEvidence(null); setError(failure instanceof Error ? failure.message : String(failure)); }
      }
    }
    void read();
    return () => {
      ++generation.current; if (timer) clearTimeout(timer);
      void transport!({ ...target, operation: "pause_view" }).catch(failure => console.error("Desktop viewer detach failed", failure));
    };
  }, [active, identity, refresh]);

  async function control(operation: DesktopSurfaceRequest["operation"]) {
    const transport = window.octopusTransport?.desktopSurfaceRequest;
    if (!surface || !transport || surface.readOnly || (controlPending.current && (operation !== "stop" || controlPending.current.operation === "stop"))) return;
    if (operation !== "stop" && !evidence) return;
    const pending = { operation }; controlPending.current = pending; setPendingControl(operation); setWarning(false);
    const current = generation.current; ++requestGeneration.current;
    const targetIdentity = identity;
    const isCurrent = () => controlPending.current === pending && (operation === "stop" ? activeIdentity.current === targetIdentity : generation.current === current);
    try {
      const result = await transport({ viewId: viewId.current, workerEnvironmentId: environment.id, surfaceId: surface.id, runId: environment.runId, operation,
        ...(operation === "stop" ? {} : { controlGeneration: evidence!.control.generation }) });
      if (!isCurrent()) return;
      if (!("evidence" in result)) throw new Error("Desktop control returned no acknowledgement evidence");
      if (operation === "stop" && result.evidence.control.mode !== "stopped") throw new Error("Desktop Stop response did not confirm stopped control");
      setEvidence(result.evidence);
      if (operation !== "stop") { setError(""); setRefresh(value => value + 1); }
    } catch (failure) { if (isCurrent()) setError(failure instanceof Error ? failure.message : String(failure)); }
    finally { if (controlPending.current === pending) { controlPending.current = null; setPendingControl(null); } }
  }
  function openSettings() {
    setPreferences({ settingsSection: "computer-use" });
    dispatchClientCommand("open_settings", "surface", { sectionId: "computer-use" });
  }
  if (surface?.mode === "fixture") return <div className="we-desktop-shell"><div className="we-desktop-fixture">
    <div className="we-desktop-fixture-toolbar"><span /><span /><span /><strong>{dataText(surface.data, "title") || surface.label}</strong></div>
    <div><Glyph name="desktop" size={28} /><strong>{dataText(surface.data, "detail") || "Test desktop room"}</strong><small>No host desktop is connected.</small></div>
  </div></div>;
  if (!surface || !window.octopusTransport?.desktopSurfaceRequest) return <LiveSurfaceEmpty glyph="desktop" title="Computer use unavailable" detail="This task has no connected application view." />;
  const mode = evidence?.control.mode;
  const permissionsMissing = evidence && (!evidence.permissions.screen_capture || !evidence.permissions.accessibility);
  const state = mode === "stopped" ? "Stopped" : mode === "human" ? "You have control" : "Computer use";
  return <div className="we-browser-shell we-desktop-application-shell" data-control={mode || "unknown"}>
    <div className="we-browser-toolbar we-desktop-application-toolbar">
      <span className="we-desktop-application-heading"><Glyph name="desktop" size={16} />
        {presentations.length > 1 ? <OcSelect label="Application" onChange={setSelected} value={application?.source_id ?? ""}
          options={presentations.map(value => ({ value: value.source_id, label: value.window_title ? `${value.display_name} · ${value.window_title}` : value.display_name }))} />
          : <span>{application?.display_name ?? state}</span>}
      </span>
      <div className="we-browser-history">
        <DesktopToolbarButton glyph="settings" label="Computer use settings" onClick={openSettings} />
        {mode === "human" || mode === "stopped"
          ? <DesktopToolbarButton disabled={busy || surface.readOnly} glyph="resume" label="Release control" onClick={() => void control("release_control")} selected />
          : <DesktopToolbarButton disabled={busy || surface.readOnly || !evidence} glyph="hand" label="Take control" onClick={() => setWarning(true)} />}
        <DesktopToolbarButton disabled={pendingControl === "stop" || surface.readOnly || (mode === "stopped" && !busy)} glyph="stop" label="Stop computer use" onClick={() => void control("stop")} />
      </div>
    </div>
    <div className="we-desktop-native-viewport">
      {application?.phase === "published" && !permissionsMissing && mode !== "stopped"
        ? <NativeApplicationView key={`${identity}:${application.source_id}`} active={active} application={application} environment={environment} surface={surface} retry={refresh} />
        : <div className="we-desktop-permission">
          <LiveSurfaceEmpty glyph="desktop" title={permissionsMissing ? "Set up computer use" : mode === "stopped" ? "Computer use stopped" : "Waiting for an app"}
            detail={permissionsMissing ? "Enable computer use and allow access in Settings." : mode === "stopped" ? "You can resume when you’re ready." : "The app this task uses will appear here."} />
          {permissionsMissing ? <OcButton onClick={openSettings} size="sm" variant="secondary">Open Settings</OcButton> : null}
        </div>}
    </div>
    {error || inventoryError || application?.error ? <div className="we-desktop-view-error" role="alert"><span>Application view unavailable.</span>
      <details><summary>Error details</summary>{error || inventoryError || application?.error}</details>
      <OcButton onClick={() => setRefresh(value => value + 1)} size="sm" variant="ghost">Try again</OcButton></div> : null}
    <ManualRoomControlWarning glyph="desktop" onCancel={() => setWarning(false)} onContinue={() => void control("take_control")} open={warning} title="Take control of the desktop" />
  </div>;
}

function NativeApplicationView({ active, application, environment, surface, retry }: {
  active: boolean; application: DesktopApplicationPresentation; environment: WorkerEnvironmentProjection; surface: WorkerEnvironmentSurfaceProjection;
  retry: number;
}) {
  const frame = useRef<HTMLDivElement>(null);
  const [error, setError] = useState("");
  const [attached, setAttached] = useState(false);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    const transport = window.octopusTransport?.desktopApplicationView;
    if (!active || !frame.current) return;
    if (!transport) { setAttached(false); setError("Native application preview is unavailable."); return; }
    setError(""); setAttached(false);
    const node = frame.current, target = { workerEnvironmentId: environment.id, surfaceId: surface.id, runId: environment.runId, viewId: crypto.randomUUID() };
    let cancelled = false, previous = "", scheduled = 0, revision = 0, failed = false;
    const report = (failure: unknown, expectedRevision: number) => { if (!cancelled && revision === expectedRevision) { failed = true; setAttached(false); setError(failure instanceof Error ? failure.message : String(failure)); } };
    const update = () => {
      cancelAnimationFrame(scheduled);
      scheduled = requestAnimationFrame(() => {
        if (cancelled || failed) return;
        const bounds = node.getBoundingClientRect(), style = getComputedStyle(node);
        const occluded = Array.from(document.querySelectorAll<HTMLElement>('[role="dialog"], [role="menu"], [role="listbox"], [data-radix-popper-content-wrapper], .we-download-peek'))
          .some(overlay => {
            if (overlay.contains(node) || overlay.closest('[hidden], [inert], [aria-hidden="true"]')) return false;
            const rect = overlay.getBoundingClientRect(), style = getComputedStyle(overlay);
            return rect.width > 0 && rect.height > 0 && style.display !== "none" && style.visibility !== "hidden" && style.opacity !== "0"
              && rect.left < bounds.right && rect.right > bounds.left && rect.top < bounds.bottom && rect.bottom > bounds.top;
          });
        if (document.visibilityState === "hidden" || !node.isConnected || style.display === "none" || style.visibility === "hidden"
          || node.closest('[inert], [aria-hidden="true"], [hidden]') || occluded
          || bounds.width <= 0 || bounds.height <= 0 || bounds.x < 0 || bounds.y < 0 || bounds.right > window.innerWidth + .5 || bounds.bottom > window.innerHeight + .5) {
          if (previous === "hidden") return;
          previous = "hidden"; const requestRevision = ++revision; setAttached(false);
          void transport({ ...target, operation: "hide" }).catch(failure => report(failure, requestRevision)); return;
        }
        const rect = { x: bounds.x, y: bounds.y, width: bounds.width, height: bounds.height }, geometry = JSON.stringify(rect);
        if (geometry === previous) return; previous = geometry;
        const requestRevision = ++revision;
        void transport({ ...target, operation: "attach", appInstanceId: application.app_instance_id, sourceWindowId: application.source_window_id, rect })
          .then(value => { if (!cancelled && revision === requestRevision) {
            if (!value.attached) throw new Error("Native application preview did not confirm attachment.");
            setAttached(true); setError("");
          } })
          .catch(failure => report(failure, requestRevision));
      });
    };
    const resize = new ResizeObserver(update); resize.observe(node);
    const mutations = new MutationObserver(update); mutations.observe(document.body, { attributes: true, attributeFilter: ["class","style","hidden","inert","aria-hidden"], childList: true, subtree: true });
    window.addEventListener("resize", update); window.addEventListener("scroll", update, true); document.addEventListener("visibilitychange", update); update();
    return () => {
      cancelled = true; cancelAnimationFrame(scheduled); resize.disconnect(); mutations.disconnect();
      window.removeEventListener("resize", update); window.removeEventListener("scroll", update, true); document.removeEventListener("visibilitychange", update);
      void transport({ ...target, operation: "detach" }).catch(failure => console.error("Application preview detach failed", failure));
    };
  }, [active, application.source_id, environment.id, environment.runId, surface.id, retry, attempt]);
  return <div className="we-desktop-application-view" ref={frame} aria-label={`${application.display_name} application view`} data-attached={attached || undefined}>
    {!attached ? <LiveSurfaceEmpty glyph="desktop" title={error ? "Application view interrupted" : "Connecting to app"} detail={error || application.display_name} /> : null}
    {error ? <OcButton onClick={() => setAttempt(value => value + 1)} size="sm" variant="ghost">Try again</OcButton> : null}
  </div>;
}

function DesktopToolbarButton({ disabled = false, glyph, label, onClick, selected = false }: {
  disabled?: boolean; glyph: GlyphName; label: string; onClick: () => void; selected?: boolean;
}) {
  return <ShellTooltip label={label}><button aria-label={label} aria-pressed={selected || undefined} className="we-browser-control" disabled={disabled} onClick={onClick} type="button"><Glyph name={glyph} size={16} /></button></ShellTooltip>;
}
