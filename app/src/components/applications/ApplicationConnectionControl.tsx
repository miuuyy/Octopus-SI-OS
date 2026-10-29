import { useRef, useState } from "react";
import { OcButton, OcMenuItem, cx } from "../../design-system/components";
import { OcDropdown } from "../../design-system/OcDropdown";
import { Glyph } from "../IconGlyphs";
import { OctopusSpinner } from "../navigation/OctopusSpinner";
import type { ApplicationConnectMode } from "./ApplicationConnectDialog";
import type { ApplicationConnectionSummary } from "./applicationModel";

export function ApplicationConnectionControl({
  connectAvailable,
  disconnecting,
  name,
  onCheck,
  onConnect,
  onDisconnect,
  placement,
  summary,
}: {
  connectAvailable: boolean;
  disconnecting: boolean;
  name: string;
  onCheck: () => void;
  onConnect: (mode: ApplicationConnectMode) => void;
  onDisconnect: () => void;
  placement: "card" | "detail";
  summary: ApplicationConnectionSummary;
}) {
  const triggerRef = useRef<HTMLButtonElement>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const unavailableTitle = connectAvailable ? undefined : "Connect applications from the Octopus desktop client.";
  if (summary.status === "none") return null;
  if (summary.status === "checking") {
    return (
      <OcButton aria-label={`Checking ${name} connection`} className="shrink-0 justify-center" disabled size="sm" variant="outline">
        <OctopusSpinner className="icon-xxs" />
      </OcButton>
    );
  }
  if (summary.status === "not_connected" && !disconnecting) {
    return (
      <OcButton
        className="shrink-0"
        disabled={!connectAvailable}
        onClick={(event) => {
          event.stopPropagation();
          onConnect("connect");
        }}
        size="sm"
        title={unavailableTitle}
        variant={placement === "detail" ? "primary" : "outline"}
      >
        Connect
      </OcButton>
    );
  }
  const disconnectable = summary.moduleInterfaces.length > 0
    || summary.fields.some((field) => field.private_saved && summary.fieldInterfaces.includes(field.interface_id));
  const label = disconnecting
    ? "Disconnecting…"
    : summary.status === "connected"
      ? "Connected"
      : summary.status === "expired"
        ? "Reconnect required"
        : "Connection issue";
  return (
    <>
      <OcButton
        aria-busy={disconnecting || undefined}
        buttonRef={triggerRef}
        className="shrink-0 gap-1.5 text-sm"
        disabled={disconnecting}
        onClick={(event) => {
          event.stopPropagation();
          setMenuOpen(true);
        }}
        size="sm"
        title={summary.status === "error" ? summary.message : undefined}
        variant="outline"
      >
        <span className="flex items-center gap-1.5">
          {disconnecting ? <OctopusSpinner className="icon-xxs" /> : (
            <span aria-hidden="true" className={cx(
              "size-1.5 shrink-0 rounded-full",
              summary.status === "connected" ? "bg-token-charts-green" : summary.status === "expired" ? "bg-token-charts-orange" : "bg-token-charts-red",
            )} />
          )}
          {label}
          {disconnecting ? null : <Glyph className="icon-2xs shrink-0 text-token-description-foreground" name="chevronDown" size={12} />}
        </span>
      </OcButton>
      <OcDropdown align="end" anchorRef={triggerRef} contentWidth="menu" onOpenChange={setMenuOpen} open={menuOpen}>
        {summary.status === "error" ? (
          <OcMenuItem label="Check again" leadingIcon="retry" onClick={() => { setMenuOpen(false); onCheck(); }} role="menuitem" />
        ) : null}
        <OcMenuItem
          disabled={!connectAvailable}
          label="Reconnect"
          leadingIcon="refresh"
          onClick={() => { setMenuOpen(false); onConnect("reconnect"); }}
          role="menuitem"
        />
        <OcMenuItem
          disabled={!connectAvailable || !disconnectable}
          label="Disconnect"
          leadingIcon="logOut"
          onClick={() => { setMenuOpen(false); onDisconnect(); }}
          role="menuitem"
          tone="danger"
        />
      </OcDropdown>
    </>
  );
}
