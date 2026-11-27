import type { GlyphName } from "../components/IconGlyphs";
import type { OctopusClientAccessProfile } from "../electron";
import type { ClientSurfaceDescriptor } from "./surfaceTypes";
import { profileStorageKey } from "./clientWorkspaces";

export type ClientNavigationSection = "home" | "tasks" | "dashboard" | "inbox" | "applications" | "drivers";

export type ClientNavigationDestination = Readonly<{
  id: ClientNavigationSection;
  label: string;
  surfaceId: string;
  glyph: GlyphName;
  selectedGlyph: GlyphName;
  sidebar: boolean;
}>;

export const NAVIGATION_RAIL_WIDTH = 0;

export const clientNavigationDestinations: readonly ClientNavigationDestination[] = [
  { id: "home", label: "Home", surfaceId: "voice", glyph: "home", selectedGlyph: "homeFilled", sidebar: false },
  { id: "tasks", label: "Tasks", surfaceId: "chat", glyph: "tasks", selectedGlyph: "tasksFilled", sidebar: true },
  { id: "inbox", label: "Inbox", surfaceId: "inbox", glyph: "bell", selectedGlyph: "bellFilled", sidebar: true },
  { id: "applications", label: "Applications", surfaceId: "applications", glyph: "applicationsRail", selectedGlyph: "applicationsRailFilled", sidebar: false },
  { id: "drivers", label: "Drivers", surfaceId: "drivers", glyph: "driversRail", selectedGlyph: "driversRailFilled", sidebar: false },
  { id: "dashboard", label: "Dashboard", surfaceId: "dashboard", glyph: "dashboardRail", selectedGlyph: "dashboardRailFilled", sidebar: false },
];

export function navigationSectionForSurface(surface: Pick<ClientSurfaceDescriptor, "id" | "placements">): ClientNavigationSection {
  switch (surface.id) {
    case "voice": return "home";
    case "chat":
    case "scheduled": return "tasks";
    case "inbox": return "inbox";
    case "applications": return "applications";
    case "drivers": return "drivers";
    default: return surface.placements.includes("home") ? "tasks" : "dashboard";
  }
}

export function navigationSectionLabel(surface: Pick<ClientSurfaceDescriptor, "id" | "placements">): string {
  const section = navigationSectionForSurface(surface);
  return clientNavigationDestinations.find((destination) => destination.id === section)!.label;
}

export function activeDestinationStorageKey(profile: OctopusClientAccessProfile): string {
  return profileStorageKey("octopus.shell.activeDestination.v1", profile);
}
