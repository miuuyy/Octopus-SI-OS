import { lazy, Suspense, useEffect, useState, type ComponentProps, type ComponentType } from "react";
import { AnimatePresence, motion, useIsPresent, useReducedMotion } from "motion/react";
import { useHomeRunActivity } from "../voice/useHomeActivityWorkspace";
import type { SurfaceHostProps } from "./SurfaceHost";
import { SurfaceLoadingState } from "./SurfaceBoundary";
import { WorkspacePaneHeaderActions, WorkspacePaneHeaderActionsProvider } from "./WorkspacePaneHeaderActions";
import { HomeWhatsNew } from "./HomeWhatsNew";
import { ShellTooltip } from "./ShellTooltip";
import { Glyph } from "../IconGlyphs";

const VoiceView = lazy(() => import("../voice/VoiceView").then((module) => ({ default: module.VoiceView })));
const loadHomeAgentActivityView = () => import("../voice/HomeAgentActivityView").then((module) => ({ default: module.HomeAgentActivityView }));
const HomeAgentActivityView = lazy(loadHomeAgentActivityView);

type HomeActivityViewProps = ComponentProps<typeof HomeAgentActivityView>;

type Props = Pick<SurfaceHostProps,
  "accessProfile"
  | "commandsActive"
  | "homeRunId"
  | "voiceSession"
  | "homeViewMode"
  | "onHomeViewModeChange"
  | "searchFocusTick"
  | "chatWorkspace"
  | "onOpenFiles"
  | "voiceSurfaces"
  | "installedVoiceWidgetIds"
  | "installedVoiceWidgetsReady"
  | "builtinVoiceWidgetSources"
  | "workerEnvironmentSurfaces"
  | "topology"
>;

export function HomeSurface({
  accessProfile,
  commandsActive = true,
  homeRunId,
  voiceSession,
  homeViewMode,
  onHomeViewModeChange,
  searchFocusTick,
  chatWorkspace,
  onOpenFiles,
  voiceSurfaces,
  installedVoiceWidgetIds,
  installedVoiceWidgetsReady,
  builtinVoiceWidgetSources,
  workerEnvironmentSurfaces,
  topology,
}: Props) {
  const homeActivity = useHomeRunActivity(homeRunId, true);
  const activityOpen = homeViewMode === "activity";
  const [widgetsOpen, setWidgetsOpen] = useState(false);
  const [swapActionHost, setSwapActionHost] = useState<HTMLSpanElement | null>(null);
  const [loadedActivityView, setLoadedActivityView] = useState<ComponentType<HomeActivityViewProps> | null>(null);
  useEffect(() => { if (activityOpen) setWidgetsOpen(false); }, [activityOpen]);
  const reducedMotion = useReducedMotion();
  const transition = { duration: reducedMotion ? 0 : 0.22, ease: [0.23, 1, 0.32, 1] as const };
  useEffect(() => {
    let current = true;
    void loadHomeAgentActivityView()
      .then((module) => { if (current) setLoadedActivityView(() => module.default); })
      .catch(() => undefined);
    return () => { current = false; };
  }, []);
  return (
    <div className="home-view-transition">
      <WorkspacePaneHeaderActions>
        <ShellTooltip label="Activity" side="right">
          <button aria-label="Activity" aria-expanded={activityOpen} className="app-titlebar-action no-drag"
            data-state={activityOpen ? "open" : "closed"} type="button" onClick={() => {
              setWidgetsOpen(false);
              onHomeViewModeChange?.(activityOpen ? "voice" : "activity");
            }}><Glyph name="transcript" size={16} /></button>
        </ShellTooltip>
        <span className="home-pane-swap-action" ref={setSwapActionHost}>
          {!activityOpen ? (
            <ShellTooltip label="Home widgets">
              <button aria-label="Home widgets" aria-expanded={widgetsOpen} className="app-titlebar-action no-drag"
                data-state={widgetsOpen ? "open" : "closed"} data-titlebar-surface-action="home-widgets" type="button" onClick={() => {
                  onHomeViewModeChange?.("voice");
                  setWidgetsOpen((open) => !open);
                }}><Glyph name="widgets" size={16} /></button>
            </ShellTooltip>
          ) : null}
        </span>
      </WorkspacePaneHeaderActions>
      <motion.div
        animate={{ opacity: activityOpen ? 0 : 1 }}
        aria-hidden={activityOpen}
        className="home-view-transition__surface"
        inert={activityOpen}
        initial={false}
        transition={transition}
      >
        <HomeWhatsNew accessProfile={accessProfile} />
        <VoiceView accessProfile={accessProfile} activity={homeActivity} builtinWidgetSources={builtinVoiceWidgetSources} commandsActive={commandsActive && homeViewMode === "voice"} homeRunId={homeRunId} installedWidgetIds={installedVoiceWidgetIds} installedWidgetsReady={installedVoiceWidgetsReady} onViewModeChange={onHomeViewModeChange} onWidgetsOpenChange={setWidgetsOpen} session={voiceSession} surfaces={voiceSurfaces} topology={topology} widgetsOpen={widgetsOpen} workspace={chatWorkspace} viewMode="voice" />
      </motion.div>
      <div aria-hidden={!activityOpen} className="home-view-transition__surface" inert={!activityOpen}>
        <WorkspacePaneHeaderActionsProvider host={activityOpen ? swapActionHost : null}>
          <AnimatePresence initial={false}>
            {activityOpen ? (
              <motion.div
                animate={{ opacity: 1 }}
                className="home-view-transition__surface"
                exit={{ opacity: 0 }}
                initial={{ opacity: 0 }}
                key="agent-activity"
                transition={transition}
              >
                <Suspense fallback={<SurfaceLoadingState label="Loading Home activity" />}>
                  <HomeActivityContent
                    commandsActive={commandsActive && homeViewMode === "activity"}
                    onOpenFiles={onOpenFiles}
                    searchFocusTick={searchFocusTick}
                    surfaces={workerEnvironmentSurfaces}
                    topology={topology}
                    view={loadedActivityView}
                    workspace={chatWorkspace}
                  />
                </Suspense>
              </motion.div>
            ) : null}
          </AnimatePresence>
        </WorkspacePaneHeaderActionsProvider>
      </div>
    </div>
  );
}

function HomeActivityContent({ view, ...props }: HomeActivityViewProps & { view: ComponentType<HomeActivityViewProps> | null }) {
  const present = useIsPresent();
  const [Activity] = useState(() => view ?? HomeAgentActivityView);
  return <Activity {...props} commandsActive={props.commandsActive && present} />;
}
