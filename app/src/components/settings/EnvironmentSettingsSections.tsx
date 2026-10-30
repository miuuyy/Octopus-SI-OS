import { settingsTargetProps } from "./settingsCatalog";
import { useClientPreferences, useSetClientPreferences, type WorkerEnvironmentDefaultPanel } from "../../clientPreferences";
import { OcCard, OcGroup, OcSelect, OcSettingsRow, OcStatus, OcSwitch } from "../../design-system/components";
import type { OctopusClientAccessProfile } from "../../electron";
import { SettingsHeading } from "./SettingsHeading";
import { GestureManager } from "./GestureManager";

export function GesturesSection({ accessProfile }: { accessProfile: OctopusClientAccessProfile }) {
  return (
    <section {...settingsTargetProps("gestures.controls")} className="settings-section">
      <SettingsHeading
        description="Move through Octopus with your hands. Video is processed on this device and never leaves it."
        title="Gestures"
      />
      <GestureManager accessProfile={accessProfile} />
    </section>
  );
}

export function WorkerEnvironmentSettingsSection() {
  const preferences = useClientPreferences();
  const setClientPreferences = useSetClientPreferences();
  return (
    <section className="settings-section">
      <SettingsHeading
        description="How projected browser, review, desktop, and custom surfaces open in the shared Worker Environment viewer."
        title="Worker Environments"
      />

      <OcGroup title="Viewer" description="Local tab and preview behavior; Worker Environment lifecycle remains projected read-only state.">
        <OcCard>
          <OcSettingsRow {...settingsTargetProps("worker-environments.default")}
            control={(
              <OcSelect<WorkerEnvironmentDefaultPanel>
                label="Default surface"
                onChange={(workerEnvironmentDefaultPanel) => setClientPreferences({ workerEnvironmentDefaultPanel })}
                options={[
                  { value: "auto", label: "Best available" },
                  { value: "browser", label: "Browser" },
                  { value: "changes", label: "Changes" },
                ]}
                value={preferences.workerEnvironmentDefaultPanel}
              />
            )}
            description="Used only when no saved layout or explicit deep-link request selects a tab."
            label="Default surface"
          />
          <OcSettingsRow {...settingsTargetProps("worker-environments.restore")}
            control={(
              <OcSwitch
                checked={preferences.restoreWorkerEnvironmentLayouts}
                label="Restore tabs and layout"
                onCheckedChange={(restoreWorkerEnvironmentLayouts) => setClientPreferences({ restoreWorkerEnvironmentLayouts })}
              />
            )}
            description="Remember open tabs, active tab, preview state, ordering, and PR split size per Worker Environment."
            label="Restore tabs and layout"
          />
          <OcSettingsRow {...settingsTargetProps("worker-environments.preview")}
            control={(
              <OcSwitch
                checked={preferences.previewWorkerEnvironmentFiles}
                label="Preview files on single click"
                onCheckedChange={(previewWorkerEnvironmentFiles) => setClientPreferences({ previewWorkerEnvironmentFiles })}
              />
            )}
            description="Single click reuses one italic preview tab; double click pins a permanent file tab."
            label="File preview tabs"
          />
        </OcCard>
      </OcGroup>

      <OcGroup title="Safety" description="Live controls remain gated by the exact projected surface descriptor.">
        <OcCard>
          <OcSettingsRow {...settingsTargetProps("worker-environments.browser-control")}
            control={<OcStatus label="Always ask" tone="success" />}
            description="Interactive CDP control requires a deliberate confirmation each time a live browser surface is opened."
            label="Manual browser control"
          />
          <OcSettingsRow {...settingsTargetProps("worker-environments.direct-viewers")}
            control={<OcStatus label="Descriptor bounded" tone="success" />}
            description="Filesystem reads, terminal sockets, and desktop viewers resolve only from the selected Worker Environment surface."
            label="Direct viewers"
          />
        </OcCard>
      </OcGroup>
    </section>
  );
}
