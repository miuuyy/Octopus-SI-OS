import { useMemo } from "react";
import { useClientSurfaceDocuments } from "../client-surfaces/useClientSurfaceDocuments";
import { uniqueExtensionSurfaces } from "../client-surfaces/documentRegistry";
import { useClientExtensions } from "../extensions/useClientExtensions";
import {
  builtinClientExtensionSurfaceIds,
  materializeBuiltinClientExtensionSurface,
} from "../extensions/builtinSurfaceContributions";
import type { AdmittedClientSurfaceDocument } from "../client-surfaces/types";
import type { ClientSurfaceDescriptor, SurfaceRegistryState } from "./surfaceTypes";

export const builtinSurfaceDescriptors: ClientSurfaceDescriptor[] = [
  {
    id: "dashboard",
    label: "Topology",
    glyph: "topology",
    rendererKind: "builtin.dashboard",
    order: 10,
    source: "builtin",
    placements: ["shell"],
    chrome: {
      topbar: "dashboard",
      workspace: "dashboard",
      titlebarGlyph: "topology",
      searchPlaceholder: "Search EEs, runs, interfaces...",
      usesTopology: true,
    },
  },
  {
    id: "registry",
    label: "Registry",
    glyph: "sites",
    rendererKind: "builtin.registry",
    order: 20,
    source: "builtin",
    placements: ["shell"],
    chrome: {
      topbar: "wide",
      workspace: "panel",
      titlebarGlyph: "sites",
      searchPlaceholder: "Search artifacts, marks, and records...",
    },
  },
  {
    id: "worker-environments",
    label: "Worker Environments",
    glyph: "ee",
    rendererKind: "builtin.worker_environments",
    order: 30,
    source: "builtin",
    placements: ["shell"],
    chrome: {
      topbar: "none",
      workspace: "immersive",
      titlebarGlyph: "ee",
      usesTopology: true,
    },
  },
  {
    id: "event-log",
    label: "Movements",
    glyph: "worktree",
    rendererKind: "builtin.event_log",
    order: 40,
    source: "builtin",
    placements: ["shell"],
    chrome: {
      topbar: "wide",
      workspace: "panel",
      titlebarGlyph: "worktree",
      searchPlaceholder: "Search movements, artifacts, interfaces...",
    },
  },
  {
    id: "applications",
    label: "Applications",
    glyph: "storefront",
    rendererKind: "builtin.applications",
    order: 65,
    source: "builtin",
    placements: ["home"],
    chrome: {
      topbar: "none",
      workspace: "immersive",
    },
  },
  {
    id: "drivers",
    label: "Drivers",
    glyph: "plugins",
    rendererKind: "builtin.drivers",
    order: 70,
    source: "builtin",
    placements: ["home"],
    chrome: {
      topbar: "none",
      workspace: "immersive",
    },
  },
  {
    id: "chat",
    label: "Tasks",
    glyph: "tasks",
    rendererKind: "builtin.chat",
    order: 50,
    source: "builtin",
    placements: ["home"],
    chrome: {
      topbar: "none",
      workspace: "immersive",
      usesTopology: true,
    },
  },
  {
    id: "voice",
    label: "Home",
    glyph: "home",
    rendererKind: "builtin.voice",
    order: 55,
    source: "builtin",
    placements: ["home"],
    chrome: {
      topbar: "none",
      workspace: "immersive",
      usesTopology: true,
    },
  },
  {
    id: "inbox",
    label: "Inbox",
    glyph: "bell",
    rendererKind: "builtin.inbox",
    order: 58,
    source: "builtin",
    placements: ["home"],
    chrome: {
      topbar: "none",
      workspace: "immersive",
      titlebarGlyph: "bell",
    },
  },
  {
    id: "scheduled",
    label: "Triggers",
    glyph: "scheduled",
    rendererKind: "builtin.scheduled",
    order: 60,
    source: "builtin",
    placements: ["home"],
    chrome: {
      topbar: "none",
      workspace: "immersive",
    },
  },
];

export function useClientSurfaces(accessProfile: "live" | "lab"): SurfaceRegistryState {
  const documents = useClientSurfaceDocuments(accessProfile);
  const extensions = useClientExtensions(accessProfile);
  return useMemo(() => {
    const errors = [...documents.errors, ...extensions.errors];
    const builtinExtensionSurfaces = extensions.extensions.flatMap((extension) => {
      if (!extension.installed || !extension.configured || extension.manifest.category !== "tab") return [];
      const contribution = extension.manifest.contribution;
      if (contribution.kind !== "builtin") return [];
      const surface = materializeBuiltinClientExtensionSurface(extension.manifest.id, contribution.surface_id);
      if (surface) return [surface];
      errors.push(`built-in tab extension "${extension.manifest.id}" has no surface contribution for "${contribution.surface_id}"`);
      return [];
    });
    const builtinIds = new Set([
      ...builtinSurfaceDescriptors.map((surface) => surface.id),
      ...builtinClientExtensionSurfaceIds,
    ]);
    const builtinConflictDigests = new Set<string>();
    const workspaceSurfaces = uniqueExtensionSurfaces(
      [...documents.workspaceSurfaces, ...extensions.workspaceSurfaces],
      "workspace",
      errors,
    );
    const customSurfaces = workspaceSurfaces.flatMap((admitted) => {
      const document = admitted.document;
      const target = document.target;
      if (target.kind !== "workspace.surface") return [];
      if (builtinIds.has(document.id)) {
        errors.push(`${admitted.sourcePath}: client surface id "${document.id}" conflicts with a builtin surface`);
        builtinConflictDigests.add(admitted.sourceDigest);
        return [];
      }
      return [{
        id: document.id,
        label: document.title,
        glyph: document.glyph,
        rendererKind: "custom.document" as const,
        order: target.order,
        source: "client" as const,
        placements: target.placements,
        chrome: {
          topbar: target.chrome.topbar,
          workspace: target.chrome.workspace,
          titlebarGlyph: target.chrome.titlebar_glyph,
          searchPlaceholder: target.chrome.search_placeholder,
          usesTopology: Boolean(document.sources?.length),
        },
        document: admitted,
      }];
    });
    customSurfaces.sort((left, right) => left.order - right.order || left.label.localeCompare(right.label));
    return {
      loading: documents.loading || extensions.loading,
      hydrated: documents.hydrated && extensions.hydrated,
      surfaces: [...builtinSurfaceDescriptors, ...builtinExtensionSurfaces, ...customSurfaces],
      documents: [...documents.documents, ...extensions.workspaceSurfaces, ...extensions.voiceSurfaces],
      voiceSurfaces: uniqueExtensionSurfaces(
        [...documents.voiceSurfaces, ...extensions.voiceSurfaces],
        "voice",
        errors,
        true,
      ),
      workerEnvironmentSurfaces: documents.workerEnvironmentSurfaces,
      documentRoot: documents.root,
      documentVerdicts: documents.verdicts.map((verdict) => (
        builtinConflictDigests.has(verdict.details.source_digest)
          ? {
              passed: false,
              details: {
                ...verdict.details,
                issues: [...verdict.details.issues, {
                  code: "registry.id.builtin_conflict",
                  path: "/id",
                  message: `document id "${verdict.details.document_id}" conflicts with a builtin surface`,
                }],
              },
            }
          : verdict
      )),
      errors,
      extensions,
    };
  }, [documents.documents, documents.errors, documents.hydrated, documents.loading, documents.root, documents.verdicts, documents.voiceSurfaces, documents.workerEnvironmentSurfaces, documents.workspaceSurfaces, extensions]);
}
