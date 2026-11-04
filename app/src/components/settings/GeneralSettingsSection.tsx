import { settingsTargetProps } from "./settingsCatalog";
import { useEffect, useMemo, useState } from "react";
import { useClientPreferences, useSetClientPreferences, type SendShortcut } from "../../clientPreferences";
import { OcButton, OcCard, OcGroup, OcSelect, OcSlider, OcSettingsRow, OcStatus, OcSwitch } from "../../design-system/components";
import { type GlyphName } from "../IconGlyphs";
import type { OctopusClientAccessProfile } from "../../electron";
import { useAppErrorToast } from "../../toasts";
import { loadChatSettings, saveChatSettings } from "../chat/storage";
import { providerDefaultModel, providerDefaultReasoningEffort } from "../chat/chatWorkspaceCapabilities";
import type { ClientSurfaceDescriptor } from "../../shell/surfaceTypes";
import { clientNavigationDestinations } from "../../shell/navigation";
import { SettingsHeading } from "./SettingsHeading";
import { MarkActivitySettingsRow, NotificationSoundSettingsRow } from "./NotificationSettingsSection";

import { ProviderSummary, useDeclaredProviders } from "./SettingsProviderState";
import { readProjection, saveSetupFile } from "../../projectionClient";
import { serializeSetupFile, setupFileFromHome, setupFileName } from "../../onboarding/setupFile";
import { useClientExtensions } from "../../extensions/useClientExtensions";
export function GeneralSection({ accessProfile, surfaces }: { accessProfile: OctopusClientAccessProfile; surfaces: ClientSurfaceDescriptor[] }) {
  const preferences = useClientPreferences();
  const setClientPreferences = useSetClientPreferences();
  const providerState = useDeclaredProviders();
  const [providerInterfaceId, setProviderInterfaceId] = useState(
    () => loadChatSettings(accessProfile).providerInterfaceId,
  );
  const [loginItem, setLoginItem] = useState<{ supported: boolean; enabled: boolean; loading: boolean; error: string }>({
    supported: false,
    enabled: false,
    loading: accessProfile === "live",
    error: "",
  });
  const storedProviderMissing = Boolean(
    !providerState.loading && !providerState.error && providerInterfaceId
      && !providerState.providers.some((provider) => provider.id === providerInterfaceId),
  );
  const extensions = useClientExtensions(accessProfile);
  const [setupExport, setSetupExport] = useState<{ busy: boolean; note: string; error: string }>({ busy: false, note: "", error: "" });
  useAppErrorToast(providerState.error || loginItem.error || setupExport.error);

  async function exportHomeSetup() {
    setSetupExport({ busy: true, note: "", error: "" });
    try {
      const [scope, localPackages, applications, drivers] = await Promise.all([
        readProjection("projection.operator.os.scope"),
        readProjection("projection.operator.os.local_packages"),
        readProjection("projection.operator.os.applications"),
        readProjection("projection.operator.os.drivers"),
      ]);
      const file = setupFileFromHome({
        scope,
        localPackages,
        applications,
        drivers,
        installedExtensionIds: new Set(extensions.extensions.filter((extension) => extension.installed).map((extension) => extension.manifest.id)),
      });
      const saved = await saveSetupFile({ text: serializeSetupFile(file), suggestedName: setupFileName(file) });
      setSetupExport({ busy: false, note: saved.saved ? `Saved ${saved.name}.` : "", error: "" });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setSetupExport({ busy: false, note: "", error: message.replace(/^Error invoking remote method '[^']+': (?:Error: )?/, "") || "Octopus could not export this setup." });
    }
  }

  useEffect(() => {
    if (accessProfile !== "live" || !window.octopusTransport?.loginItemSettings) return;
    let cancelled = false;
    window.octopusTransport.loginItemSettings()
      .then((state) => {
        if (!cancelled) setLoginItem({ ...state, loading: false, error: "" });
      })
      .catch((error) => {
        if (!cancelled) setLoginItem({ supported: false, enabled: false, loading: false, error: error instanceof Error ? error.message : String(error) });
      });
    return () => { cancelled = true; };
  }, [accessProfile]);

  async function setOpenAtLogin(enabled: boolean) {
    const bridge = window.octopusTransport;
    if (!bridge?.loginItemSettings) return;
    setLoginItem((current) => ({ ...current, loading: true, error: "" }));
    try {
      const state = await bridge.loginItemSettings({ enabled });
      setLoginItem({ ...state, loading: false, error: "" });
    } catch (error) {
      setLoginItem((current) => ({ ...current, loading: false, error: error instanceof Error ? error.message : String(error) }));
    }
  }
  const providerOptions = useMemo(() => {
    const options = [
      { value: "", label: "Inherit from Home" },
      ...providerState.providers.map((provider) => ({ value: provider.id, label: provider.label })),
    ];
    if (storedProviderMissing) {
      return [{ value: providerInterfaceId, label: `${providerInterfaceId} · not declared`, disabled: true }, ...options];
    }
    return options;
  }, [providerInterfaceId, providerState.providers, storedProviderMissing]);
  const selectedProvider = providerState.providers.find((provider) => provider.id === providerInterfaceId);
  const startupSurfaceOptions = useMemo(() => {
    const options: Array<{ value: string; label: string; glyph: GlyphName; disabled?: boolean }> = clientNavigationDestinations
      .filter((destination) => surfaces.some((surface) => surface.id === destination.surfaceId))
      .map((destination) => ({ value: destination.surfaceId, label: destination.label, glyph: destination.glyph }));
    for (const surface of surfaces) {
      if (options.some((option) => option.value === surface.id)) continue;
      options.push({ value: surface.id, label: surface.label, glyph: surface.glyph });
    }
    if (!options.some((option) => option.value === preferences.startupSurfaceId)) {
      options.unshift({ value: preferences.startupSurfaceId, label: `${preferences.startupSurfaceId} · unavailable`, glyph: "descriptor", disabled: true });
    }
    return options;
  }, [preferences.startupSurfaceId, surfaces]);

  function selectProvider(next: string) {
    const provider = providerState.providers.find((candidate) => candidate.id === next);
    const model = provider ? providerDefaultModel(provider) : null;
    const reasoningEffort = provider && model
      ? providerDefaultReasoningEffort(provider, model.model)
      : "";
    setProviderInterfaceId(next);
    saveChatSettings({
      providerInterfaceId: next,
      providerModel: model?.model ?? "",
      reasoningEffort,
    }, accessProfile);
  }

  return (
    <section className="settings-section">
      <SettingsHeading
        description="Desktop behavior that is stored locally for this client."
        title="General"
      />

      <OcGroup title="Startup & tasks" description="Choose the launch destination and task display preferences.">
        <OcCard>
          <OcSettingsRow {...settingsTargetProps("general.first-surface")}
            control={(
              <OcSelect<string>
                label="Open on launch"
                onChange={(startupSurfaceId) => setClientPreferences({ startupSurfaceId })}
                options={startupSurfaceOptions}
                value={preferences.startupSurfaceId}
              />
            )}
            description="Choose the launch destination. The last opened surface is restored when available."
            label="Open on launch"
          />
          <OcSettingsRow {...settingsTargetProps("general.internal-runs")}
            control={(
              <OcSwitch
                checked={preferences.showInternalRuns}
                label="Show internal runs"
                onCheckedChange={(showInternalRuns) => setClientPreferences({ showInternalRuns })}
              />
            )}
            description="Include runs inside the Octopus OS scope that have no exact client interface. They open read-only and show artifact movement activity."
            label="Internal runs"
          />
          <OcSettingsRow {...settingsTargetProps("general.task-pane-width")}
            control={(
              <OcSlider
                className="settings-slider-control"
                formatValue={(value) => `${value}%`}
                label="Task pane width"
                max={68}
                min={28}
                onChange={(secondaryTaskPaneWidthPercent) => setClientPreferences({ secondaryTaskPaneWidthPercent })}
                step={1}
                value={preferences.secondaryTaskPaneWidthPercent}
              />
            )}
            description="Set the default width of the task pane. Dragging the divider updates the same preference."
            label="Task pane width"
          />
        </OcCard>
      </OcGroup>

      <OcGroup title="Composer" description="These controls change the local chat client.">
        <OcCard>
          <OcSettingsRow {...settingsTargetProps("general.send-shortcut")}
            control={(
              <OcSelect<SendShortcut>
                label="Send shortcut"
                onChange={(sendShortcut) => setClientPreferences({ sendShortcut })}
                options={[
                  { value: "enter", label: "Enter" },
                  { value: "cmd-if-multiline", label: "⌘ Enter for multiline prompts" },
                  { value: "cmd-always", label: "⌘ Enter always" },
                ]}
                value={preferences.sendShortcut}
              />
            )}
            description="Choose when Enter sends a prompt or inserts a new line."
            label="Send shortcut"
          />
          <OcSettingsRow {...settingsTargetProps("general.spell-check")}
            control={(
              <OcSwitch
                checked={preferences.spellcheckEnabled}
                label="Spell check"
                onCheckedChange={(spellcheckEnabled) => setClientPreferences({ spellcheckEnabled })}
              />
            )}
            description="Use the operating system dictionary and suggestions in the task composer."
            label="Spell check"
          />
          <OcSettingsRow {...settingsTargetProps("general.active-reply")}
            control={<span className="text-sm text-token-text-secondary">Steer current reply</span>}
            description="Send steers the current reply immediately. To queue a message, right-click Send or use the queue shortcut."
            label="While a run is active"
          />
          <OcSettingsRow {...settingsTargetProps("general.work-mode")}
            control={(
              <OcSelect<"detailed" | "concise">
                label="Technical detail"
                onChange={(technicalDetailLevel) => setClientPreferences({ technicalDetailLevel })}
                options={[
                  { value: "detailed", label: "Detailed" },
                  { value: "concise", label: "Concise" },
                ]}
                value={preferences.technicalDetailLevel}
              />
            )}
            description="Detailed shows projected reasoning, tools, plans, searches, and hooks inline. Concise keeps the same evidence available inside a collapsed activity group."
            label="Work mode"
          />
        </OcCard>
      </OcGroup>

      <OcGroup
        title="Default provider interface"
        description="New tasks inherit Home's declared provider unless you explicitly choose another interface here."
      >
        <OcCard>
          <OcSettingsRow {...settingsTargetProps("general.provider")}
            control={(
              <OcSelect<string>
                className="settings-provider-select"
                disabled={providerState.loading || providerOptions.length === 0}
                error={providerState.error || (storedProviderMissing ? "This saved interface is unavailable. Choose a declared interface or Inherit from Home." : undefined)}
                label="Default provider interface"
                onChange={selectProvider}
                options={providerOptions}
                placeholder={providerState.loading ? "Reading registry…" : "Select an interface"}
                value={providerInterfaceId}
              />
            )}
            description={providerInterfaceId
              ? "This exact interface id is stored as the chat default."
              : "No override is stored. New tasks inherit the provider and reasoning effort projected from Home."}
            label="Provider"
          />
          {selectedProvider ? <ProviderSummary provider={selectedProvider} compact /> : null}
        </OcCard>
      </OcGroup>

      <OcGroup title="Application" description="Launch behavior for the desktop client.">
        <OcCard>
          <OcSettingsRow {...settingsTargetProps("general.on-launch")}
            control={<OcStatus label="Restore last surface" tone="neutral" />}
            description="The client reopens the surface that was active when it closed."
            label="On launch"
          />
          <OcSettingsRow {...settingsTargetProps("general.open-at-login")}
            control={accessProfile === "live" ? (
              <OcSwitch
                checked={loginItem.enabled}
                disabled={loginItem.loading || !loginItem.supported}
                label="Open at login"
                onCheckedChange={(enabled) => void setOpenAtLogin(enabled)}
              />
            ) : <OcStatus label="Administrator only" tone="muted" />}
            description="Ask the operating system to launch the packaged Octopus application after sign-in."
            label="Open at login"
            meta={accessProfile === "live" && !loginItem.loading && !loginItem.supported ? "Available in packaged builds." : undefined}
          />
          <OcSettingsRow {...settingsTargetProps("general.keep-awake")}
            control={(
              <OcSwitch
                checked={preferences.keepAwakeDuringRuns}
                label="Keep awake during runs"
                onCheckedChange={(keepAwakeDuringRuns) => setClientPreferences({ keepAwakeDuringRuns })}
              />
            )}
            description="Prevent the system from sleeping while a run is executing."
            label="Keep awake"
          />
        </OcCard>
      </OcGroup>
      {accessProfile === "live" ? (
        <OcGroup title="Setup" description="Share this Home's structure or start another Octopus from it.">
          <OcCard>
            <OcSettingsRow {...settingsTargetProps("general.export-setup")}
              control={(
                <OcButton busy={setupExport.busy} disabled={setupExport.busy || extensions.loading} onClick={() => void exportHomeSetup()}>
                  Export setup…
                </OcButton>
              )}
              description="Save the space, its projects and the capabilities each project uses as a file. Another Octopus imports it on the first step of setup. Keys, accounts, tasks and history stay here."
              label="Setup file"
              meta={setupExport.note || undefined}
            />
          </OcCard>
        </OcGroup>
      ) : null}
      <OcGroup title="Notifications">
        <OcCard>
          <MarkActivitySettingsRow />
          <NotificationSoundSettingsRow />
        </OcCard>
      </OcGroup>
    </section>
  );
}
