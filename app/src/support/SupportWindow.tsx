import { useEffect, useRef, useState } from "react";
import { OcButton, OcCheckbox, OcTextArea } from "../design-system/components";

export type SupportReport = {
  report_id: string;
  captured_at: string;
  access: "live" | "lab" | "locked";
  application: { version: string; revision: string | null; platform: string; arch: string };
  [key: string]: unknown;
};
type ReportAction = { reportId: string; description: string };
type ReportSource = { kind: "current" | "saved"; name: string };
declare global {
  interface Window {
    octopusSupport?: {
      collect(includeDetails: boolean, source?: "choose" | "current"): Promise<{ report: SupportReport; source: ReportSource } | null>;
      copy(request: ReportAction): Promise<{ copied: boolean }>;
      save(request: ReportAction): Promise<{ saved: boolean }>;
      openIssue(request: ReportAction): Promise<{ opened: boolean }>;
    };
  }
}

export function SupportWindow() {
  const [report, setReport] = useState<SupportReport | null>(null);
  const [source, setSource] = useState<ReportSource>({ kind: "current", name: "Current Home" });
  const [description, setDescription] = useState("");
  const [includeDetails, setIncludeDetails] = useState(false);
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const generation = useRef(0);

  async function collect(details = includeDetails, selectedSource?: "choose" | "current") {
    const attempt = ++generation.current;
    setBusy(true); setError(""); setNotice("");
    if (selectedSource !== "choose") setReport(null);
    try {
      if (!window.octopusSupport) throw new Error("Problem reports are available in the Octopus desktop app.");
      const result = await window.octopusSupport.collect(details, selectedSource);
      if (attempt === generation.current && result) { setReport(result.report); setSource(result.source); }
    } catch (failure) {
      if (attempt === generation.current) setError(failure instanceof Error ? failure.message : String(failure));
    } finally { if (attempt === generation.current) setBusy(false); }
  }

  useEffect(() => { void collect(false); return () => { generation.current++; }; }, []);

  async function act(action: "copy" | "save" | "openIssue") {
    if (!report || !window.octopusSupport) return;
    setBusy(true); setError(""); setNotice("");
    try {
      const result = await window.octopusSupport[action]({ reportId: report.report_id, description });
      setNotice(action === "copy" ? "Report copied." : action === "openIssue" ? "Draft opened in your browser. Review and submit it on GitHub." : "saved" in result && result.saved ? "Report saved. You can attach it to your GitHub issue." : "");
    } catch (failure) { setError(failure instanceof Error ? failure.message : String(failure)); }
    finally { setBusy(false); }
  }

  return (
    <main className="flex h-screen flex-col bg-token-main-surface-primary text-token-foreground">
      <div className="min-h-0 flex-1 space-y-5 overflow-y-auto p-6">
        <header>
          <h1 className="text-xl font-semibold">Report a problem</h1>
          <p className="mt-2 text-sm text-token-description-foreground">Tell us what happened. Review your diagnostics before sharing them.</p>
        </header>
        <OcTextArea autoFocus label="What went wrong?" showLabel textareaClassName="min-h-32" id="support-description" maxLength={4000} onChange={(event) => setDescription(event.target.value)} placeholder="What were you doing, and what did you expect to happen?" value={description} />
        <section aria-label="Diagnostics" className="space-y-3">
          <div className="flex items-center justify-between gap-2">
            <h2 className="font-medium">Diagnostics</h2>
            <OcButton disabled={busy} onClick={() => void collect()} size="sm" variant="ghost">Refresh</OcButton>
          </div>
          <div className="space-y-2">
            <p className="break-all text-sm">{source.kind === "saved" ? `Saved Home: ${source.name}` : source.name}</p>
            <div className="flex flex-wrap gap-2">
              <OcButton disabled={busy || report?.access !== "live"} onClick={() => void collect(includeDetails, "choose")} size="sm" variant="secondary">Choose saved Home…</OcButton>
              {source.kind === "saved" ? <OcButton disabled={busy} onClick={() => void collect(includeDetails, "current")} size="sm" variant="ghost">Use current Home</OcButton> : null}
            </div>
            {source.kind === "saved" ? <p className="text-sm text-token-description-foreground">Reads this Home without opening or changing it. Build details identify the app collecting the report.</p> : null}
          </div>
          <p className="text-sm text-token-description-foreground">Build, setup, recent errors and runtime diagnostics. Conversations, tool contents and project files are excluded. Nothing is sent automatically.</p>
          <OcCheckbox checked={includeDetails} disabled={busy || (!includeDetails && report?.access !== "live")} label="Include error text and log excerpts" onCheckedChange={(checked) => { setIncludeDetails(checked); void collect(checked); }} showLabel />
          {includeDetails ? <p className="text-sm text-token-description-foreground">Error text can contain private paths, request content or secrets. Read the preview before sharing.</p> : null}
          {report && report.access !== "live" ? <p className="text-sm text-token-description-foreground">This report contains application facts only. To include setup and logs, unlock Octopus as Administrator and reopen this window.</p> : null}
          {report ? <details className="rounded-xl border border-token-border p-3">
            <summary className="cursor-pointer text-sm">Preview diagnostic data</summary>
            <pre className="mt-3 max-h-64 overflow-auto whitespace-pre-wrap break-words font-mono text-xs select-text">{JSON.stringify(report, null, 2)}</pre>
          </details> : <p aria-live="polite" className="text-sm text-token-description-foreground">{busy ? "Collecting diagnostics…" : "No report collected."}</p>}
          {report ? <p className="text-xs text-token-description-foreground">Captured {new Date(report.captured_at).toLocaleTimeString()} · {report.application.version} · {report.application.revision?.slice(0, 8) ?? "Build revision not recorded"}</p> : null}
        </section>
        {error ? <p role="alert" className="text-sm text-danger">{error}</p> : null}
        <p aria-live="polite" className="text-sm text-token-description-foreground">{notice}</p>
      </div>
      <footer className="shrink-0 space-y-3 border-t border-token-border p-5">
        <p className="text-xs text-token-description-foreground">GitHub opens a public issue draft with your description and build details. Attach the saved report there if you want to share diagnostics.</p>
        <div className="flex flex-wrap justify-end gap-2">
          <OcButton disabled={busy || !report} onClick={() => void act("copy")} variant="ghost">Copy report</OcButton>
          <OcButton disabled={busy || !report} onClick={() => void act("save")} variant="secondary">Save report…</OcButton>
          <OcButton disabled={busy || !report} onClick={() => void act("openIssue")}>Open GitHub draft</OcButton>
        </div>
      </footer>
    </main>
  );
}
