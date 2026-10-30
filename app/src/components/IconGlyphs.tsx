import { Suspense, type ReactNode } from "react";

export type GlyphName =
  | "octopus"
  | "home"
  | "homeFilled"
  | "tasks"
  | "tasksFilled"
  | "dashboardRail"
  | "dashboardRailFilled"
  | "inbox"
  | "inboxFilled"
  | "applicationsRail"
  | "applicationsRailFilled"
  | "driversRail"
  | "driversRailFilled"
  | "dashboard"
  | "registry"
  | "metrics"
  | "usage"
  | "dollar"
  | "chat"
  | "interfaces"
  | "topology"
  | "ee"
  | "artifact"
  | "file"
  | "read"
  | "download"
  | "binding"
  | "descriptor"
  | "filter"
  | "search"
  | "expand"
  | "restore"
  | "more"
  | "close"
  | "copy"
  | "trash"
  | "send"
  | "steer"
  | "sendNow"
  | "edit"
  | "drag"
  | "plus"
  | "microphone"
  | "microphoneOff"
  | "voice"
  | "voiceMode"
  | "volume"
  | "volumeOff"
  | "phoneEnd"
  | "transcript"
  | "weather"
  | "deviceTablet"
  | "deviceMobile"
  | "rotateDevice"
  | "terminal"
  | "browser"
  | "browserUse"
  | "changes"
  | "desktop"
  | "retry"
  | "resume"
  | "stop"
  | "play"
  | "pause"
  | "statusIdle"
  | "playCircle"
  | "pauseCircle"
  | "paperclip"
  | "image"
  | "settings"
  | "sliders"
  | "chevronDown"
  | "chevronUp"
  | "check"
  | "refresh"
  | "compact"
  | "openExternal"
  | "arrowLeft"
  | "navigateBack"
  | "sidebarOpen"
  | "sidebarClosed"
  | "user"
  | "users"
  | "lock"
  | "sun"
  | "keyboard"
  | "pulse"
  | "newTask"
  | "scheduled"
  | "archive"
  | "plugins"
  | "tool"
  | "sites"
  | "pin"
  | "pinOff"
  | "bell"
  | "activity"
  | "activityUnread"
  | "clearRead"
  | "activityOptions"
  | "hand"
  | "flask"
  | "bug"
  | "warning"
  | "dataControls"
  | "environment"
  | "taskSummary"
  | "taskPlan"
  | "viewAll"
  | "project"
  | "briefcase"
  | "code"
  | "globe"
  | "rocket"
  | "palette"
  | "storefront"
  | "book"
  | "lightbulb"
  | "folder"
  | "folderOpen"
  | "worktree"
  | "logOut"
  | "splitPane"
  | "widgets"
  | "layers"
  | "progress"
  | "insights"
  | "memory"
  | "target";

type GlyphPackName = "shell" | "navigation" | "chat" | "worker" | "voice" | "settings";

type Props = {
  name: GlyphName;
  className?: string;
  size?: number;
};

export type GlyphDef = {
  viewBox?: string;
  node: ReactNode;
  sizes?: Readonly<Record<number, { viewBox: string; node: ReactNode }>>;
};

const glyphs = new Map<GlyphName, GlyphDef>();
const glyphPackOwners = new Map<GlyphName, GlyphPackName>();
const glyphLoadPromises = new Map<GlyphName, Promise<void>>();

const glyphPackByName: Record<GlyphName, GlyphPackName> = {
  homeFilled: "navigation",
  tasks: "navigation",
  tasksFilled: "navigation",
  dashboardRail: "navigation",
  dashboardRailFilled: "navigation",
  inbox: "navigation",
  inboxFilled: "navigation",
  applicationsRail: "navigation",
  applicationsRailFilled: "navigation",
  driversRail: "navigation",
  driversRailFilled: "navigation",
  octopus: "shell",
  home: "shell",
  dashboard: "shell",
  registry: "shell",
  metrics: "shell",
  usage: "shell",
  dollar: "shell",
  interfaces: "shell",
  topology: "shell",
  ee: "shell",
  artifact: "shell",
  file: "shell",
  newTask: "shell",
  scheduled: "shell",
  archive: "navigation",
  plugins: "shell",
  sites: "shell",
  storefront: "shell",
  worktree: "shell",
  copy: "shell",
  edit: "shell",
  plus: "shell",
  search: "shell",
  refresh: "shell",
  openExternal: "shell",
  filter: "shell",
  more: "shell",
  settings: "shell",
  sliders: "shell",
  trash: "shell",
  expand: "shell",
  restore: "shell",
  close: "shell",
  chevronDown: "shell",
  chevronUp: "shell",
  check: "shell",
  arrowLeft: "shell",
  navigateBack: "shell",
  sidebarOpen: "shell",
  sidebarClosed: "shell",
  lock: "shell",
  lightbulb: "shell",
  folder: "shell",
  folderOpen: "shell",
  logOut: "shell",
  splitPane: "shell",
  pulse: "shell",
  viewAll: "shell",
  pin: "shell",
  pinOff: "shell",
  chat: "shell",
  terminal: "shell",
  browser: "shell",
  browserUse: "chat",
  changes: "shell",
  microphone: "shell",
  voice: "shell",
  paperclip: "shell",
  phoneEnd: "shell",
  project: "shell",
  warning: "shell",
  environment: "shell",
  taskSummary: "shell",
  taskPlan: "shell",
  user: "shell",
  users: "shell",
  send: "chat",
  steer: "chat",
  sendNow: "chat",
  drag: "chat",
  stop: "chat",
  resume: "chat",
  image: "chat",
  compact: "chat",
  read: "chat",
  download: "chat",
  binding: "chat",
  descriptor: "chat",
  tool: "chat",
  briefcase: "chat",
  code: "chat",
  globe: "chat",
  rocket: "chat",
  palette: "chat",
  book: "chat",
  hand: "shell",
  retry: "worker",
  play: "worker",
  pause: "worker",
  statusIdle: "worker",
  playCircle: "worker",
  pauseCircle: "worker",
  desktop: "worker",
  deviceTablet: "worker",
  deviceMobile: "worker",
  rotateDevice: "worker",
  microphoneOff: "voice",
  voiceMode: "voice",
  volume: "voice",
  volumeOff: "voice",
  transcript: "voice",
  weather: "voice",
  target: "voice",
  memory: "voice",
  layers: "voice",
  progress: "voice",
  insights: "voice",
  bell: "settings",
  activity: "shell",
  activityUnread: "shell",
  clearRead: "shell",
  activityOptions: "shell",
  keyboard: "settings",
  sun: "settings",
  dataControls: "settings",
  widgets: "settings",
  flask: "settings",
  bug: "settings",
};

const glyphPackLoaders: Record<GlyphPackName, () => Promise<unknown>> = {
  shell: () => Promise.resolve(),
  navigation: () => import("./icon-glyphs/navigation"),
  chat: () => import("./icon-glyphs/chat"),
  worker: () => import("./icon-glyphs/worker"),
  voice: () => import("./icon-glyphs/voice"),
  settings: () => import("./icon-glyphs/settings"),
};

export function registerGlyphPack(owner: GlyphPackName, pack: Partial<Record<GlyphName, GlyphDef>>) {
  for (const [name, glyph] of Object.entries(pack) as Array<[GlyphName, GlyphDef]>) {
    const existingOwner = glyphPackOwners.get(name);
    if (existingOwner && existingOwner !== owner) throw new Error("Glyph " + name + " is registered by more than one pack");
    glyphPackOwners.set(name, owner);
    glyphs.set(name, glyph);
  }
}

export function Glyph(props: Props) {
  return (
    <Suspense fallback={<GlyphFrame {...props} />}>
      <ResolvedGlyph {...props} />
    </Suspense>
  );
}

function ResolvedGlyph(props: Props) {
  const glyph = glyphs.get(props.name);
  if (!glyph) throw loadGlyph(props.name);
  return <GlyphFrame {...props} glyph={glyph} />;
}

function GlyphFrame({ name, className = "", size = 16, glyph }: Props & { glyph?: GlyphDef }) {
  const shape = glyph?.sizes?.[size] ?? glyph;
  return (
    <svg
      className={("glyph glyph-" + name + " " + className).trim()}
      width={size}
      height={size}
      viewBox={shape?.viewBox ?? "0 0 24 24"}
      aria-hidden="true"
      focusable="false"
    >
      {shape?.node}
    </svg>
  );
}

function loadGlyph(name: GlyphName): Promise<void> {
  const current = glyphLoadPromises.get(name);
  if (current) return current;
  const promise = glyphPackLoaders[glyphPackByName[name]]().then(() => {
    if (!glyphs.has(name)) throw new Error("Glyph pack did not register " + name);
  });
  glyphLoadPromises.set(name, promise);
  return promise;
}

export const strokeProps = {
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.65,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
};
