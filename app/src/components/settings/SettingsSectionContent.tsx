import type { ReactNode } from "react";
import type { AdmittedClientSurfaceDocument, ClientSurfaceAdmissionVerdict } from "../../client-surfaces/types";
import type { OctopusClientAccessProfile } from "../../electron";
import type { ClientExtensionRegistryState } from "../../extensions/types";
import type { ClientSurfaceDescriptor } from "../../shell/surfaceTypes";
import { AppearanceSection } from "./AppearanceSection";
import { AccountSettingsSection } from "./AccountSettingsSection";
import { ComputerUseSettingsSection } from "./ComputerUseSettingsSection";
import { ExperimentsSection } from "./ExperimentsSection";
import { ExtensionsSection, type ClientExtensionDestination } from "./ExtensionsSection";
import {
  AboutSection,
  AccessSection,
  ConnectionsSection,
  GeneralSection,
  GesturesSection,
  ShortcutsSection,
  TestLabSection,
  VoiceAudioSection,
  WorkerEnvironmentSettingsSection,
} from "./OperationalSections";
import { SurfacesSection } from "./SurfacesSection";
import type { SettingsSectionId } from "./settingsCatalog";
import { settingsTargetProps } from "./settingsCatalog";

export type SettingsSectionContext = {
  dictationRecordingId?: string;
  onShowAllRecordings?: () => void;
  accessProfile: OctopusClientAccessProfile;
  documentRoot: string;
  documentVerdicts: ClientSurfaceAdmissionVerdict[];
  documents: AdmittedClientSurfaceDocument[];
  extensions: ClientExtensionRegistryState;
  onOpenExtensionDestination: (destination: ClientExtensionDestination) => void;
  surfaceErrors: string[];
  surfaces: ClientSurfaceDescriptor[];
};

const SETTINGS_SECTION_CONTENT: Record<SettingsSectionId, (context: SettingsSectionContext) => ReactNode> = {
  general: ({ accessProfile, surfaces }) => <GeneralSection accessProfile={accessProfile} surfaces={surfaces} />,
  account: () => <AccountSettingsSection />,
  appearance: () => <div {...settingsTargetProps("appearance.theme")}><AppearanceSection /></div>,
  "voice-audio": ({ accessProfile, dictationRecordingId, onShowAllRecordings }) => <VoiceAudioSection accessProfile={accessProfile} recordingId={dictationRecordingId} onShowAllRecordings={onShowAllRecordings} />,
  "computer-use": () => <ComputerUseSettingsSection />,
  gestures: ({ accessProfile }) => <GesturesSection accessProfile={accessProfile} />,
  shortcuts: ({ accessProfile }) => <ShortcutsSection accessProfile={accessProfile} />,
  access: ({ accessProfile }) => <AccessSection accessProfile={accessProfile} />,
  extensions: ({ accessProfile, extensions, onOpenExtensionDestination }) => (
    <div {...settingsTargetProps("extensions.catalog")}>
      <ExtensionsSection accessProfile={accessProfile} onOpenDestination={onOpenExtensionDestination} registry={extensions} />
    </div>
  ),
  experiments: ({ accessProfile }) => <ExperimentsSection accessProfile={accessProfile} />,
  connections: () => <ConnectionsSection />,
  surfaces: ({ accessProfile, documentRoot, documents, surfaceErrors, surfaces }) => (
    <div {...settingsTargetProps("surfaces.sidebar")}>
      <SurfacesSection
        accessProfile={accessProfile}
        documentRoot={documentRoot}
        documents={documents}
        surfaceErrors={surfaceErrors}
        surfaces={surfaces}
      />
    </div>
  ),
  "worker-environments": () => <WorkerEnvironmentSettingsSection />,
  about: () => <AboutSection />,
  "test-lab": ({ accessProfile, documentRoot, documentVerdicts, surfaceErrors }) => (
    <TestLabSection
      accessProfile={accessProfile}
      documentRoot={documentRoot}
      documentVerdicts={documentVerdicts}
      surfaceErrors={surfaceErrors}
    />
  ),
};

export function SettingsSectionContent({
  sectionId,
  ...context
}: SettingsSectionContext & { sectionId: SettingsSectionId }) {
  return SETTINGS_SECTION_CONTENT[sectionId](context);
}
