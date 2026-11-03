import { OcLoadingRows } from "../design-system/LoadingSkeleton";
import type { DragEndEvent } from "@dnd-kit/core";
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type FormEvent, type KeyboardEvent, type MouseEvent as ReactMouseEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useFloatingMenu } from "../design-system/useFloatingMenu";
import type { ChatExecutionContext, ChatRun } from "../chatRuns";
import {
  visibleClientAgents,
  type ClientAgentDefinition,
  type ClientAgentPresetInput,
} from "../agentPresets";
import {
  clientProjectScopeIds,
  clientTaskTitle,
  clientTaskUnread,
  ensureClientTaskReadBaseline,
  flattenClientChatProjects,
  recordClientTaskOpened,
  setClientSidebarSectionCollapsed,
  setClientTaskSortMode,
  useClientTaskOrganization,
  type ClientChatProject,
  type ClientTaskSortMode,
} from "../shell/clientWorkspaces";
import type { ClientSurfaceDescriptor } from "../shell/surfaceTypes";
import { navigationSectionForSurface, type ClientNavigationSection } from "../shell/navigation";
import { appToast, useAppErrorToast } from "../toasts";
import { OcButton, OcIconButton, OcDialog, OcDialogFooter, OcDialogHeader, OcMenuItem, OcMenuSurface, OcSelect, OcTextField } from "../design-system/components";
import { TasksViewSwitcher } from "./task/TasksViewSwitcher";
import { ScheduledSidebar } from "./scheduled/ScheduledSidebar";
import { Glyph } from "./IconGlyphs";
import { OctopusAgentRow, OctopusNavItem, OctopusNavSection, OctopusProjectRow, OctopusTaskRow } from "./navigation/OctopusNav";
import { projectWorkingTaskCounts } from "./navigation/sidebarActivity";
import { taskHoverCardDetails } from "./navigation/TaskHoverCard";
import {
  SidebarDndContext,
  SidebarDropContainer,
  SidebarSortableItem,
  SidebarSortableList,
  sidebarItemDndId,
  type SidebarDragData,
} from "./navigation/SidebarDnd";
import type { RuntimeHostView } from "../runtimeHost";
import type { OctopusOsProjectWorkspace } from "../shell/useOctopusOsProjects";
import { ProjectionResourceIssue, type ProjectionResourcePhase } from "./projectionResource";
import { ProjectSourceFolders } from "./project/ProjectSourceFolders";
import { ProjectAppearancePicker } from "./navigation/ProjectAppearancePicker";
import type { OctopusProviderInterface } from "../electron";
import { AgentPresetDialog } from "./agent/AgentPresetDialog";
import { AgentPresetLogo } from "./agent/AgentPresetLogo";
import { PROJECT_ICON_OPTIONS, ProjectLogo } from "./project/ProjectLogo";
import { DEFAULT_PROJECT_COLOR, PROJECT_COLOR_OPTIONS } from "../design-system/projectColors";
import { allowedProjectPolicies, type ProjectPolicyOption } from "../shell/projectPolicyOptions";
import {
  dispatchOpenClientTaskActionsMenu,
  dispatchRenameClientTask,
  dispatchRestoreClientTask,
} from "../shell/taskActionsMenu";

type Props = {
  accessProfile: "live" | "lab";
  surfaces: ClientSurfaceDescriptor[];
  navigationSection: ClientNavigationSection;
  activeSurfaceId: string;
  activeChatRunId: string;
  chatRuns: ChatRun[];
  chatRunsPhase: ProjectionResourcePhase;
  chatRunListError: string;
  onRetryChatRuns: () => Promise<void>;
  homeRunId: string;
  activityContent: ReactNode;
  agentPinnedChatIds: string[];
  agentChatOrderIds: string[];
  agentProjectFilterEeId: string;
  agentProjectWorkspace: OctopusOsProjectWorkspace;
  agentExecutionContexts: ChatExecutionContext[];
  agentDefinitions: readonly ClientAgentDefinition[];
  agentProviderInterfaces: OctopusProviderInterface[];
  agentModelsLoading: boolean;
  agentModelError: string;
  onRefreshAgentModels: () => Promise<void>;
  surfaceErrors: string[];
  runtimeHost: RuntimeHostView;
  onSurfaceChange: (surfaceId: string) => void;
  onSelectChat: (runId: string) => void;
  onNewChat: (projectId?: string) => void;
  onAgentPinnedChatIdsChange: (runIds: string[]) => void;
  onAgentChatOrderIdsChange: (runIds: string[]) => void;
  onAgentProjectFilterEeIdChange: (projectId: string) => void;
  onSearch: () => void;
};

const PINNED_TASKS_CONTAINER = "shell:pinned-tasks";
const PROJECTS_CONTAINER = "shell:projects";
const TASKS_CONTAINER = "shell:tasks";

type ProjectMenuState = {
  projectId: string;
  left: number;
  top: number;
};

type ProjectDialogState = {
  mode: "create" | "edit";
  projectId: string;
  parentId: string;
};

type ProjectDialogValue = {
  sourceFolders: string[];
  name: string;
  icon: string;
  color: string;
  templateName: string;
};

type AgentDialogState = {
  mode: "create" | "edit";
  presetId: string;
};

type ProjectDialogParent = {
  eeId: string;
  displayName: string;
  templateName: string;
};

export function Sidebar({
  accessProfile,
  surfaces,
  navigationSection,
  activeSurfaceId,
  activeChatRunId,
  chatRuns,
  chatRunsPhase,
  chatRunListError,
  onRetryChatRuns,
  homeRunId,
  activityContent,
  agentPinnedChatIds,
  agentChatOrderIds,
  agentProjectFilterEeId,
  agentProjectWorkspace,
  agentExecutionContexts,
  agentDefinitions,
  agentProviderInterfaces,
  agentModelsLoading,
  agentModelError,
  onRefreshAgentModels,
  surfaceErrors,
  runtimeHost,
  onSurfaceChange,
  onSelectChat,
  onNewChat,
  onAgentPinnedChatIdsChange,
  onAgentChatOrderIdsChange,
  onAgentProjectFilterEeIdChange,
  onSearch,
}: Props) {
  const inbox = navigationSection === "inbox";
  const viewScope = JSON.stringify([accessProfile, runtimeHost.home, homeRunId]);
  const [taskView, setTaskView] = useState({ scope: viewScope, archived: false });
  const activeTaskArchived = chatRuns.find((run) => run.runId === activeChatRunId)?.archived === true;
  const archivedView = taskView.scope === viewScope && taskView.archived;
  const scheduledView = !inbox && activeSurfaceId === "scheduled" && !archivedView;
  useEffect(() => {
    setTaskView({ scope: viewScope, archived: activeTaskArchived });
  }, [viewScope, activeChatRunId, activeTaskArchived]);
  useAppErrorToast(surfaceErrors.join("\n"));

  return (
    <aside className="octopus-sidebar app-shell-left-panel sidebar-page" data-app-shell-left-panel-appearance="content-surface">
      <nav className="sidebar-foreground-muted flex min-h-0 flex-1 flex-col" aria-label={`${inbox ? "Inbox" : "Tasks"} navigation`} onKeyDown={moveSidebarFocus}>
        {!inbox ? <TasksViewSwitcher
          value={archivedView ? "archived" : activeSurfaceId === "scheduled" ? "scheduled" : "tasks"}
          onChange={(view) => {
            setTaskView({ scope: viewScope, archived: view === "archived" });
            if (view === "archived") return;
            if (view === "tasks") onAgentProjectFilterEeIdChange("");
            onSurfaceChange(view === "scheduled" ? "scheduled" : "chat");
          }}
        /> : null}
        {inbox ? (
          <div className="sidebar-page-heading"><h1>Inbox</h1></div>
        ) : null}
        {scheduledView ? <ScheduledSidebar onSelectChat={onSelectChat} /> : <div className="octopus-sidebar-scroll sidebar-page-scroll vertical-scroll-fade-mask flex min-h-0 flex-1 flex-col gap-4 overflow-x-hidden overflow-y-auto px-0 pb-row-x" data-activity={inbox}>
          {inbox ? activityContent : (
            <TaskNavigation
              accessProfile={accessProfile}
              archivedView={archivedView}
              activeChatRunId={activeChatRunId}
              activeSurfaceId={activeSurfaceId}
              chatOrderIds={agentChatOrderIds}
              chatRuns={chatRuns}
              chatRunsPhase={chatRunsPhase}
              chatRunListError={chatRunListError}
              onRetryChatRuns={onRetryChatRuns}
              homeRunId={homeRunId}
              pinnedChatIds={agentPinnedChatIds}
              projectWorkspace={agentProjectWorkspace}
              projectFilterEeId={agentProjectFilterEeId}
              executionContexts={agentExecutionContexts}
              agentDefinitions={agentDefinitions}
              runtimeHome={runtimeHost.home}
              providerInterfaces={agentProviderInterfaces}
              modelsLoading={agentModelsLoading}
              modelError={agentModelError}
              onRefreshModels={onRefreshAgentModels}
              surfaces={surfaces}
              onChatOrderIdsChange={onAgentChatOrderIdsChange}
              onPinnedChatIdsChange={onAgentPinnedChatIdsChange}
              onProjectFilterEeIdChange={onAgentProjectFilterEeIdChange}
              onSelectChat={onSelectChat}
              onSurfaceChange={onSurfaceChange}
              onNewChat={onNewChat}
              onSearch={onSearch}
            />
          )}
        </div>}
      </nav>
    </aside>
  );
}

function TaskNavigation({
  accessProfile,
  archivedView,
  activeChatRunId,
  activeSurfaceId,
  chatOrderIds,
  chatRuns,
  chatRunsPhase,
  chatRunListError,
  onRetryChatRuns,
  homeRunId,
  pinnedChatIds,
  projectWorkspace,
  projectFilterEeId,
  executionContexts,
  agentDefinitions,
  runtimeHome,
  providerInterfaces,
  modelsLoading,
  modelError,
  onRefreshModels,
  surfaces,
  onChatOrderIdsChange,
  onPinnedChatIdsChange,
  onProjectFilterEeIdChange,
  onSelectChat,
  onSurfaceChange,
  onNewChat,
  onSearch,
}: {
  accessProfile: "live" | "lab";
  archivedView: boolean;
  activeChatRunId: string;
  activeSurfaceId: string;
  chatOrderIds: string[];
  chatRuns: ChatRun[];
  chatRunsPhase: ProjectionResourcePhase;
  chatRunListError: string;
  onRetryChatRuns: () => Promise<void>;
  homeRunId: string;
  pinnedChatIds: string[];
  projectWorkspace: OctopusOsProjectWorkspace;
  projectFilterEeId: string;
  executionContexts: ChatExecutionContext[];
  agentDefinitions: readonly ClientAgentDefinition[];
  runtimeHome: string;
  providerInterfaces: OctopusProviderInterface[];
  modelsLoading: boolean;
  modelError: string;
  onRefreshModels: () => Promise<void>;
  surfaces: ClientSurfaceDescriptor[];
  onChatOrderIdsChange: (runIds: string[]) => void;
  onPinnedChatIdsChange: (runIds: string[]) => void;
  onProjectFilterEeIdChange: (projectId: string) => void;
  onSelectChat: (runId: string) => void;
  onSurfaceChange: (surfaceId: string) => void;
  onNewChat: (projectId?: string) => void;
  onSearch: () => void;
}) {
  const projects = projectWorkspace.projects;
  const onProjectsChange = projectWorkspace.updatePresentation;
  useAppErrorToast(projectWorkspace.error);
  const organization = useClientTaskOrganization(accessProfile);
  const now = useSidebarClock();
  const collapsedSectionIdSet = useMemo(() => new Set(organization.collapsedSectionIds), [organization.collapsedSectionIds]);
  const pinnedCollapsed = collapsedSectionIdSet.has("pinned");
  const projectsCollapsed = collapsedSectionIdSet.has("projects");
  const agentsCollapsed = collapsedSectionIdSet.has("agents");
  const tasksCollapsed = collapsedSectionIdSet.has("tasks");
  const [projectDialog, setProjectDialog] = useState<ProjectDialogState | null>(null);
  const [agentDialog, setAgentDialog] = useState<AgentDialogState | null>(null);
  const [projectMenu, setProjectMenu] = useState<ProjectMenuState | null>(null);
  const [sortMenu, setSortMenu] = useState<{ left: number; top: number } | null>(null);
  const [deletingProjectId, setDeletingProjectId] = useState("");
  const [draggingSidebarItem, setDraggingSidebarItem] = useState(false);
  const agentSurfaceRows = surfaces
    .filter((surface) => (
      navigationSectionForSurface(surface) === "tasks"
      && !["chat", "scheduled"].includes(surface.id)
    ))
    .sort((left, right) => left.order - right.order || left.label.localeCompare(right.label));
  const chatActive = activeSurfaceId === "chat";
  const allTaskChats = useMemo(() => chatRuns.filter((chat) => chat.runId !== homeRunId), [chatRuns, homeRunId]);
  const taskChats = useMemo(() => allTaskChats.filter((chat) => !chat.archived), [allTaskChats]);
  const workingTaskCounts = useMemo(() => projectWorkingTaskCounts(projects, taskChats), [projects, taskChats]);
  const chatById = new Map(allTaskChats.map((chat) => [chat.runId, chat]));
  const projectById = new Map(projects.map((project) => [project.id, project]));
  const projectRows = useMemo(() => flattenClientChatProjects(projects), [projects]);
  const hoverDetailsByRun = useMemo(() => {
    const owners = new Map(projects.map((project) => [project.id, project]));
    return new Map(allTaskChats.map((chat) => [chat.runId, taskHoverCardDetails(chat, owners.get(chat.eeId), agentDefinitions)]));
  }, [agentDefinitions, projects, allTaskChats]);
  const hoverScope = JSON.stringify([accessProfile, runtimeHome]);
  const selectedProjectId = projectById.has(projectFilterEeId) ? projectFilterEeId : "";
  const selectedProject = selectedProjectId ? projectById.get(selectedProjectId) ?? null : null;
  const selectedScopeProjectIds = useMemo(() => {
    if (!selectedProjectId) return null;
    return clientProjectScopeIds(projects, selectedProjectId);
  }, [projects, selectedProjectId]);
  const selectedScopeRunIds = useMemo(() => {
    if (!selectedScopeProjectIds) return null;
    return new Set(projects.flatMap((project) => (
      selectedScopeProjectIds.has(project.id) ? [...project.chatRunIds, ...(project.readOnlyRunIds ?? [])] : []
    )));
  }, [projects, selectedScopeProjectIds]);
  const visibleAgentPresets = visibleClientAgents(agentDefinitions).filter((preset) => (
    (projectById.has(preset.projectEeId) || executionContexts.some((context) => context.isOsRoot && context.eeId === preset.projectEeId))
    && (!selectedScopeProjectIds || selectedScopeProjectIds.has(preset.projectEeId))
  ));
  const agentCreationAvailable = Boolean(
    accessProfile === "live"
    && providerInterfaces.length
    && (selectedProject
      ? executionContexts.some((context) => (
          context.eeId === selectedProject.id
          && context.workers.filter((worker) => worker.agentSourceTemplate).length === 1
        ))
      : executionContexts.some((context) => (
          (context.isOsRoot || projectById.has(context.eeId))
          && context.workers.filter((worker) => worker.agentSourceTemplate).length === 1
        ))),
  );
  const pinnedChats = pinnedChatIds.flatMap((runId) => {
    const chat = chatById.get(runId);
    return chat && !chat.archived && (!selectedScopeRunIds || selectedScopeRunIds.has(runId)) ? [chat] : [];
  });
  const pinnedChatSet = new Set(pinnedChats.map((chat) => chat.runId));
  const visibleChats = orderChats(
    taskChats.filter((chat) => (
      !pinnedChatSet.has(chat.runId)
      && (!selectedScopeRunIds || selectedScopeRunIds.has(chat.runId))
    )),
    chatOrderIds,
    organization.sortMode,
  );
  const archivedChats = allTaskChats.filter((chat) => (
    chat.archived && (!selectedScopeRunIds || selectedScopeRunIds.has(chat.runId))
  ));

  useEffect(() => {
    ensureClientTaskReadBaseline(accessProfile);
  }, [accessProfile]);

  const selectProject = (projectId: string) => {
    onProjectFilterEeIdChange(selectedProjectId === projectId ? "" : projectId);
  };

  const toggleSection = (sectionId: string, collapsed: boolean) => {
    setClientSidebarSectionCollapsed(accessProfile, sectionId, !collapsed);
  };

  const selectTask = (runId: string) => {
    recordClientTaskOpened(accessProfile, runId);
    onSelectChat(runId);
  };

  const openTaskMenu = (runId: string, event: ReactMouseEvent<HTMLElement>) => {
    dispatchOpenClientTaskActionsMenu(runId, event.currentTarget.getBoundingClientRect(),
      event.type === "contextmenu" ? { x: event.clientX, y: event.clientY } : undefined);
  };

  const onDragEnd = (event: DragEndEvent) => {
    setDraggingSidebarItem(false);
    if (archivedView) return;
    if (event.active.id === event.over?.id) return;
    const source = dragData(event.active.data.current);
    const target = dragTarget(event);
    if (!source || !target) return;
    if (source.entity === "project") {
      if (target.containerId !== PROJECTS_CONTAINER) return;
      const flatIds = projectRows.map((row) => row.project.id);
      onProjectsChange(orderProjects(projects, moveBefore(
        flatIds,
        source.itemId,
        target.entity === "project" ? (target.itemId ?? null) : null,
      )));
      return;
    }
    if (source.entity !== "chat") return;
    if (target.containerId === PINNED_TASKS_CONTAINER) {
      onPinnedChatIdsChange(moveBefore(
        pinnedChats.map((chat) => chat.runId),
        source.itemId,
        target.entity === "chat" ? (target.itemId ?? null) : null,
      ));
      return;
    }
    if (target.containerId === TASKS_CONTAINER) {
      onPinnedChatIdsChange(pinnedChats.map((chat) => chat.runId).filter((id) => id !== source.itemId));
      onChatOrderIdsChange(moveBefore(
        visibleChats.map((chat) => chat.runId),
        source.itemId,
        target.entity === "chat" ? (target.itemId ?? null) : null,
      ));
      setClientTaskSortMode(accessProfile, "manual");
    }
  };

  const submitProjectDialog = async (value: ProjectDialogValue) => {
    if (!projectDialog) return;
    if (projectDialog.mode === "create") {
      await projectWorkspace.createProject({
        name: value.name,
        sourceFolders: value.sourceFolders,
        icon: value.icon,
        color: value.color,
        parentId: projectDialog.parentId,
        templateName: value.templateName,
      });
    } else {
      await projectWorkspace.editProject({
        projectId: projectDialog.projectId,
        name: value.name,
        sourceFolders: value.sourceFolders,
        icon: value.icon,
        color: value.color,
      });
    }
    setProjectDialog(null);
  };

  const submitAgentDialog = async (value: ClientAgentPresetInput) => {
    if (!agentDialog) return;
    try {
      if (agentDialog.mode === "create") {
        await projectWorkspace.createAgentTemplate(value);
      } else {
        const current = agentDefinitions.find((agent) => agent.id === agentDialog.presetId);
        if (!current?.clientTemplateDigest && accessProfile === "live") {
          throw new Error(`${current?.name || value.name} has no writable client-template evidence.`);
        }
        if (!current) throw new Error(`Agent '${agentDialog.presetId}' is no longer available.`);
        await projectWorkspace.configureAgentTemplate(value, current.id, current.clientTemplateDigest);
      }
      setAgentDialog(null);
      appToast.success(agentDialog.mode === "create" ? `${value.name} created` : `${value.name} updated`);
    } catch (failure) {
      appToast.danger(failure instanceof Error ? failure.message : String(failure));
    }
  };

  const deleteAgent = async (presetId: string) => {
    try {
      const current = agentDefinitions.find((agent) => agent.id === presetId);
      if (!current) throw new Error(`Agent '${presetId}' is no longer available.`);
      if (!current.clientTemplateDigest && accessProfile === "live") {
        throw new Error(`${current.name} has no writable client-template evidence.`);
      }
      await projectWorkspace.deleteAgentTemplate(
        current.id,
        current.projectEeId,
        current.workerInterfaceId,
        current.clientTemplateDigest,
      );
      setAgentDialog(null);
      appToast.info(`${current.name} deleted`);
    } catch (failure) {
      appToast.danger(failure instanceof Error ? failure.message : String(failure));
    }
  };

  const deleteProject = async () => {
    if (!deletingProjectId || projectWorkspace.pending) return;
    try {
      const receipt = await projectWorkspace.deleteProject(deletingProjectId);
      if (selectedProjectId === deletingProjectId) {
        onProjectFilterEeIdChange("");
      }
      setDeletingProjectId("");
      if (receipt) {
        appToast.info(`${receipt.project.name} deleted`, 8, {
          dedupeKey: `lab-project-delete:${receipt.project.id}`,
          action: {
            label: "Undo",
            onSelect: () => {
              void projectWorkspace.restoreProject(receipt).catch((failure) => {
                appToast.danger(failure instanceof Error ? failure.message : String(failure));
              });
            },
          },
        });
      }
    } catch (failure) {
      appToast.danger(failure instanceof Error ? failure.message : String(failure));
    }
  };

  const openProjectMenu = (projectId: string, event: ReactMouseEvent<HTMLButtonElement>) => {
    const rect = event.currentTarget.getBoundingClientRect();
    setProjectMenu({
      projectId,
      left: Math.max(8, Math.min(rect.right - 208, window.innerWidth - 216)),
      top: Math.max(8, Math.min(rect.bottom + 4, window.innerHeight - 168)),
    });
  };

  const dialogProject = projectDialog?.mode === "edit" ? projectById.get(projectDialog.projectId) ?? null : null;
  const dialogAgent = agentDialog?.mode === "edit"
    ? agentDefinitions.find((preset) => preset.id === agentDialog.presetId) ?? null
    : null;
  const dialogParent = projectDialog?.mode === "create" && projectDialog.parentId
    ? projectById.get(projectDialog.parentId) ?? null
    : null;
  const dialogParentContext: ProjectDialogParent | null = projectDialog?.mode === "create"
    ? projectDialog.parentId === projectWorkspace.rootEeId
      ? projectWorkspace.rootContext
      : dialogParent ? {
        eeId: dialogParent.id,
        displayName: dialogParent.name,
        templateName: dialogParent.templateName ?? "",
      } : null
    : null;
  const dialogTemplateIds = projectDialog?.mode === "create"
    ? projectDialog.parentId === projectWorkspace.rootEeId
      ? projectWorkspace.rootAllowedChildTemplateIds
      : dialogParent?.allowedChildTemplateIds ?? []
    : [];
  const dialogTemplateOptions = allowedProjectPolicies(projectWorkspace.templates, dialogTemplateIds);
  const canCreateRootProject = accessProfile !== "live" || Boolean(
    projectWorkspace.rootContext && allowedProjectPolicies(projectWorkspace.templates, projectWorkspace.rootAllowedChildTemplateIds).length > 0,
  );
  const rootProjectUnavailableReason = projectWorkspace.phase === "initial-loading"
    ? "Loading project policies…"
    : projectWorkspace.phase === "failed"
      ? `Project policies could not be loaded: ${projectWorkspace.error}`
      : !canCreateRootProject
        ? "The parent has no installed policy available for a new project"
        : undefined;
  const deletingProject = deletingProjectId ? projectById.get(deletingProjectId) ?? null : null;
  const deletingProjectScopeIds = deletingProject ? clientProjectScopeIds(projects, deletingProject.id) : null;
  const deletingProjectAgentCount = deletingProjectScopeIds
    ? visibleClientAgents(agentDefinitions).filter((preset) => deletingProjectScopeIds.has(preset.projectEeId)).length
    : 0;
  const deleteBlockedReason = deletingProjectAgentCount > 0
    ? `Move or delete ${deletingProjectAgentCount === 1 ? "its agent" : `its ${deletingProjectAgentCount} agents`} first.`
    : deletingProject && accessProfile === "live"
      ? projects.some((project) => project.parentId === deletingProject.id)
        ? "Move or delete its nested execution environments first."
        : deletingProject.chatRunIds.length + (deletingProject.readOnlyRunIds?.length ?? 0) > 0
          ? "Delete every run in this execution environment first, or ask Home to move them."
          : ""
      : "";

  return (
    <SidebarDndContext
      onDragCancel={() => setDraggingSidebarItem(false)}
      onDragEnd={onDragEnd}
      onDragStart={() => setDraggingSidebarItem(true)}
      renderDragOverlay={(data) => {
        if (data.entity === "project") {
          const project = projectById.get(data.itemId);
          return project ? (
            <OctopusProjectRow
              label={project.name}
              logo={<ProjectLogo color={project.color} icon={project.icon} />}
              onClick={() => {}}
              workingTaskCount={workingTaskCounts.get(project.id)}
            />
          ) : null;
        }
        const chat = data.entity === "chat" ? chatById.get(data.itemId) : undefined;
        return chat ? (
          <OctopusTaskRow
            active={false}
            failed={chat.turnState === "failed"}
            meta={relativeTime(chat.updatedAtMs, now)}
            onClick={() => {}}
            scheduledLabel={scheduledLabelForChat(chat, now)}
            title={clientTaskTitle(chat.runId, chat.title, organization)}
            waiting={chat.turnState === "waiting"}
            working={chat.turnState === "working"}
          />
        ) : null;
      }}
    >
      {archivedView ? (
        <OctopusNavSection className="px-row-x" title="Archived" titleTrailing={archivedChats.length || undefined}>
          {archivedChats.map((chat) => <OctopusTaskRow
            action={{ glyph: "retry", label: "Restore task", disabled: projectWorkspace.pending, onClick: () => dispatchRestoreClientTask(chat.runId) }}
            active={chatActive && chat.runId === activeChatRunId}
            key={chat.runId}
            hoverDetails={hoverDetailsByRun.get(chat.runId)}
            hoverScope={hoverScope}
            meta={relativeTime(chat.updatedAtMs, now)}
            onClick={() => selectTask(chat.runId)}
            onContextMenu={(event) => openTaskMenu(chat.runId, event)}
            title={clientTaskTitle(chat.runId, chat.title, organization)}
          />)}
          <TaskListState
            empty={archivedChats.length === 0}
            emptyLabel="No archived tasks"
            error={chatRunListError}
            loadingLabel="Loading archived tasks…"
            onRetry={onRetryChatRuns}
            phase={chatRunsPhase}
          />
        </OctopusNavSection>
      ) : <>
      <div className="shrink-0 px-row-x">
        <OctopusNavSection
          collapsed={projectsCollapsed}
          title={<SidebarSectionTitle collapsed={projectsCollapsed} label="Projects" onClick={() => toggleSection("projects", projectsCollapsed)} />}
        >
          <SidebarDropContainer containerId={PROJECTS_CONTAINER} entity="project" id="sidebar-drop-projects">
            <div className="flex flex-col gap-px">
              <SidebarSortableList items={projectRows.map((row) => sidebarItemDndId("project", row.project.id))}>
                {projectRows.map(({ project, depth }) => (
                  <SidebarSortableItem
                    data={{ entity: "project", itemId: project.id, containerId: PROJECTS_CONTAINER }}
                    id={sidebarItemDndId("project", project.id)}
                    key={project.id}
                  >
                    {(dragHandle) => (
                      <OctopusProjectRow
                        action={{ glyph: "more", label: `Project actions for ${project.name}`, onClick: (event) => openProjectMenu(project.id, event) }}
                        active={project.id === selectedProjectId}
                        depth={depth}
                        dragHandle={dragHandle}
                        label={project.name}
                        logo={<ProjectLogo color={project.color} icon={project.icon} />}
                        onClick={() => selectProject(project.id)}
                        workingTaskCount={workingTaskCounts.get(project.id)}
                      />
                    )}
                  </SidebarSortableItem>
                ))}
              </SidebarSortableList>
              <button
                className="octopus-nav-row group relative flex h-[var(--height-token-row)] w-full cursor-interaction items-center overflow-x-hidden rounded-[var(--radius-token-row)] text-left text-base font-medium text-token-input-placeholder-foreground hover:bg-token-list-hover-background hover:text-token-foreground focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-token-border"
                data-sidebar-nav-item=""
                disabled={!canCreateRootProject || projectWorkspace.pending}
                onClick={() => {
                  if (canCreateRootProject) setProjectDialog({
                    mode: "create",
                    projectId: "",
                    parentId: projectWorkspace.rootEeId,
                  });
                }}
                title={rootProjectUnavailableReason}
                type="button"
              >
                <div className="flex min-w-0 flex-1 items-center gap-1 pl-1">
                  <span className="-mx-[3px] flex size-[var(--height-token-row)] shrink-0 items-center justify-center">
                    <span
                      aria-hidden="true"
                      className="flex shrink-0 items-center justify-center rounded-[6px] bg-current/10"
                      style={{ height: 20, width: 20 }}
                    >
                      <Glyph name="plus" size={13} />
                    </span>
                  </span>
                  <span className="text-fade-truncate pr-1">Add project</span>
                </div>
              </button>
            </div>
          </SidebarDropContainer>
        </OctopusNavSection>
      </div>

      <div className="shrink-0 px-row-x">
        <OctopusNavSection
          collapsed={agentsCollapsed}
          titleActions={(
            <OcIconButton
              appearance="sidebar"
              disabled={!agentCreationAvailable}
              glyph="plus"
              label={selectedProject ? `Create agent in ${selectedProject.name}` : "Create agent"}
              onClick={() => setAgentDialog({ mode: "create", presetId: "" })}
            />
          )}
          title={(
            <SidebarSectionTitle
              collapsed={agentsCollapsed}
              label="Agents"
              onClick={() => toggleSection("agents", agentsCollapsed)}
            />
          )}
        >
          {visibleAgentPresets.map((preset) => (
            <OctopusAgentRow
              active={agentDialog?.mode === "edit" && agentDialog.presetId === preset.id}
              key={preset.id}
              label={preset.name}
              logo={(
                <AgentPresetLogo
                  color={projectById.get(preset.projectEeId)?.color}
                  icon={preset.icon}
                  size={20}
                />
              )}
              meta={agentTaskMeta(preset, taskChats)}
              onClick={() => setAgentDialog({ mode: "edit", presetId: preset.id })}
            />
          ))}
          {visibleAgentPresets.length === 0 ? (
            <div className="p-2 text-base text-token-description-foreground opacity-50">
              {selectedProject ? "No agents in this project" : "No agents yet"}
            </div>
          ) : null}
        </OctopusNavSection>
      </div>

      <SidebarDropContainer
        className={pinnedChats.length > 0 ? "px-row-x" : draggingSidebarItem ? "-my-4 px-row-x" : ""}
        containerId={PINNED_TASKS_CONTAINER}
        entity="chat"
        id="sidebar-drop-agent-pinned"
      >
        {pinnedChats.length > 0 ? (
          <OctopusNavSection
            collapsed={pinnedCollapsed}
            title={<SidebarSectionTitle collapsed={pinnedCollapsed} label="Pinned" onClick={() => toggleSection("pinned", pinnedCollapsed)} />}
          >
            <SidebarSortableList items={pinnedChats.map((chat) => sidebarItemDndId("chat", chat.runId))}>
              {pinnedChats.map((chat) => (
                <SidebarSortableItem
                  data={{ entity: "chat", itemId: chat.runId, containerId: PINNED_TASKS_CONTAINER }}
                  id={sidebarItemDndId("chat", chat.runId)}
                  key={chat.runId}
                >
                  <OctopusTaskRow
                    action={{ glyph: "more", label: `Task actions for ${clientTaskTitle(chat.runId, chat.title, organization)}`, onClick: (event) => openTaskMenu(chat.runId, event) }}
                    active={chatActive && chat.runId === activeChatRunId}
                    failed={chat.turnState === "failed"}
                    hoverDetails={hoverDetailsByRun.get(chat.runId)}
                    hoverDisabled={draggingSidebarItem}
                    hoverScope={hoverScope}
                    meta={relativeTime(chat.updatedAtMs, now)}
                    onClick={() => selectTask(chat.runId)}
                    onContextMenu={(event) => openTaskMenu(chat.runId, event)}
                    onDoubleClick={() => dispatchRenameClientTask(chat.runId)}
                    scheduledLabel={scheduledLabelForChat(chat, now)}
                    subtitle={taskSubtitle(chat)}
                    title={clientTaskTitle(chat.runId, chat.title, organization)}
                    unread={clientTaskUnread(chat, organization)}
                    waiting={chat.turnState === "waiting"}
                    working={chat.turnState === "working"}
                  />
                </SidebarSortableItem>
              ))}
            </SidebarSortableList>
          </OctopusNavSection>
        ) : draggingSidebarItem ? <div className="h-4" /> : null}
      </SidebarDropContainer>

      <SidebarDropContainer className="min-w-0" containerId={TASKS_CONTAINER} entity="chat" id="sidebar-drop-tasks">
        <OctopusNavSection
          className="px-row-x"
          collapsed={tasksCollapsed}
          titleActions={(
            <div className="flex items-center gap-0.5">
              <OcIconButton appearance="sidebar" glyph="plus" label={selectedProject ? `New task in ${selectedProject.name}` : "New task"} onClick={() => onNewChat(selectedProjectId || undefined)} />
              <OcIconButton appearance="sidebar" glyph="search" label="Search tasks" onClick={onSearch} />
            </div>
          )}
          titleRowClassName="flex items-center justify-between gap-2 pr-0.5 pl-2"
          title={(
            <SidebarSectionTitle
              collapsed={tasksCollapsed}
              label="Tasks"
              onOptions={(anchor) => {
                const rect = anchor.getBoundingClientRect();
                setSortMenu({
                  left: Math.max(8, Math.min(rect.left, window.innerWidth - 244)),
                  top: Math.max(8, Math.min(rect.bottom + 4, window.innerHeight - 168)),
                });
              }}
              onClick={() => toggleSection("tasks", tasksCollapsed)}
            />
          )}
        >
          <SidebarSortableList items={visibleChats.map((chat) => sidebarItemDndId("chat", chat.runId))}>
            {visibleChats.map((chat) => (
              <SidebarSortableItem
                data={{ entity: "chat", itemId: chat.runId, containerId: TASKS_CONTAINER }}
                id={sidebarItemDndId("chat", chat.runId)}
                key={chat.runId}
              >
                <OctopusTaskRow
                  action={{ glyph: "more", label: `Task actions for ${clientTaskTitle(chat.runId, chat.title, organization)}`, onClick: (event) => openTaskMenu(chat.runId, event) }}
                  active={chatActive && chat.runId === activeChatRunId}
                  failed={chat.turnState === "failed"}
                  hoverDetails={hoverDetailsByRun.get(chat.runId)}
                  hoverDisabled={draggingSidebarItem}
                  hoverScope={hoverScope}
                  meta={relativeTime(chat.updatedAtMs, now)}
                  onClick={() => selectTask(chat.runId)}
                  onContextMenu={(event) => openTaskMenu(chat.runId, event)}
                  onDoubleClick={() => dispatchRenameClientTask(chat.runId)}
                  scheduledLabel={scheduledLabelForChat(chat, now)}
                  subtitle={taskSubtitle(chat)}
                  title={clientTaskTitle(chat.runId, chat.title, organization)}
                  unread={clientTaskUnread(chat, organization)}
                  waiting={chat.turnState === "waiting"}
                  working={chat.turnState === "working"}
                />
              </SidebarSortableItem>
            ))}
          </SidebarSortableList>
          <TaskListState
            empty={visibleChats.length === 0}
            emptyLabel={selectedProject
              ? "No tasks in this project"
              : taskChats.length === 0 ? "No tasks yet" : "All visible tasks are pinned"}
            error={chatRunListError}
            loadingLabel="Loading tasks…"
            onRetry={onRetryChatRuns}
            phase={chatRunsPhase}
          />
        </OctopusNavSection>
      </SidebarDropContainer>
      <div className="shrink-0 px-row-x">
        <div className="flex flex-col gap-px">
          {agentSurfaceRows.map((surface) => (
            <OctopusNavItem
              active={activeSurfaceId === surface.id}
              glyph={surfaceGlyph(surface)}
              key={surface.id}
              label={surface.label}
              onClick={() => onSurfaceChange(surface.id)}
            />
          ))}
        </div>
      </div>

      </>}
      <ProjectDialog
        busy={projectWorkspace.pending}
        creating={projectDialog?.mode === "create"}
        initialColor={dialogProject?.color ?? ""}
        initialIcon={dialogProject?.icon ?? ""}
        initialName={dialogProject?.name ?? ""}
        initialFolders={dialogProject?.sourceFolders ?? []}
        open={Boolean(projectDialog)}
        parent={dialogParentContext}
        runtimeBacked={accessProfile === "live"}
        submitLabel={projectDialog?.mode === "edit" ? "Save" : "Create project"}
        templateOptions={dialogTemplateOptions}
        title={projectDialog?.mode === "edit"
          ? "Edit project"
          : dialogParentContext ? `New project in ${dialogParentContext.displayName}` : "Create project"}
        onCancel={() => setProjectDialog(null)}
        onSubmit={submitProjectDialog}
      />
      <AgentPresetDialog
        busy={projectWorkspace.pending}
        contexts={executionContexts}
        initialProjectEeId={selectedProjectId || projectWorkspace.rootEeId}
        open={Boolean(agentDialog)}
        preset={dialogAgent}
        presets={agentDefinitions}
        projects={projects}
        providers={providerInterfaces}
        modelsLoading={modelsLoading}
        modelError={modelError}
        onRefreshModels={onRefreshModels}
        readOnly={accessProfile !== "live"}
        onCancel={() => setAgentDialog(null)}
        onDelete={deleteAgent}
        onSubmit={submitAgentDialog}
      />
      <ProjectActionsMenu
        canCreateInside={accessProfile !== "live" || Boolean((projectMenu ? projectById.get(projectMenu.projectId)?.allowedChildTemplateIds : [])?.length)}
        menu={projectMenu}
        project={projectMenu ? projectById.get(projectMenu.projectId) ?? null : null}
        onClose={() => setProjectMenu(null)}
        onCreateInside={(project) => {
          setProjectMenu(null);
          setProjectDialog({ mode: "create", projectId: "", parentId: project.id });
        }}
        onDelete={(projectId) => {
          setProjectMenu(null);
          setDeletingProjectId(projectId);
        }}
        onEdit={(projectId) => {
          setProjectMenu(null);
          setProjectDialog({ mode: "edit", projectId, parentId: "" });
        }}
      />
      <TaskSortMenu
        menu={sortMenu}
        sortMode={organization.sortMode}
        onClose={() => setSortMenu(null)}
        onSelect={(sortMode) => {
          setClientTaskSortMode(accessProfile, sortMode);
          setSortMenu(null);
        }}
      />
      <ProjectDeleteDialog
        blockedReason={deleteBlockedReason}
        busy={projectWorkspace.pending}
        open={Boolean(deletingProjectId)}
        project={deletingProject}
        runtimeBacked={accessProfile === "live"}
        onCancel={() => setDeletingProjectId("")}
        onDelete={deleteProject}
      />
    </SidebarDndContext>
  );
}

function agentTaskMeta(preset: ClientAgentDefinition, runs: readonly ChatRun[]): string {
  const taskCount = new Set(runs.flatMap((run) => (
    run.eeId === preset.projectEeId
    && (run.workerChoiceInterfaceId || run.workerInterfaceId) === preset.workerInterfaceId
      ? [run.runId]
      : []
  ))).size;
  return `${taskCount} ${taskCount === 1 ? "task" : "tasks"}`;
}

function ProjectActionsMenu({
  canCreateInside,
  menu,
  project,
  onClose,
  onCreateInside,
  onDelete,
  onEdit,
}: {
  canCreateInside: boolean;
  menu: ProjectMenuState | null;
  project: ClientChatProject | null;
  onClose: () => void;
  onCreateInside: (project: ClientChatProject) => void;
  onDelete: (projectId: string) => void;
  onEdit: (projectId: string) => void;
}) {
  const menuRef = useRef<HTMLDivElement>(null);
  useFloatingMenu(Boolean(menu && project), menuRef, onClose, menu);

  if (!menu || !project) return null;
  return createPortal(
    <OcMenuSurface
      aria-label={`Actions for ${project.name}`}
      data-settings-layer="true"
      className="fixed z-[100] w-52"
      role="menu"
      style={{ left: menu.left, top: menu.top } as CSSProperties}
      surfaceRef={menuRef}
    >
      <OcMenuItem disabled={!canCreateInside} label="New project inside" leadingIcon="plus" onClick={() => onCreateInside(project)} role="menuitem" />
      <OcMenuItem label="Edit project" leadingIcon="edit" onClick={() => onEdit(project.id)} role="menuitem" />
      <div aria-hidden="true" className="mx-1 my-1 h-px bg-token-border" />
      <OcMenuItem label="Delete project" leadingIcon="trash" onClick={() => onDelete(project.id)} role="menuitem" tone="danger" />
    </OcMenuSurface>,
    document.body,
  );
}

function TaskSortMenu({
  menu,
  sortMode,
  onClose,
  onSelect,
}: {
  menu: { left: number; top: number } | null;
  sortMode: ClientTaskSortMode;
  onClose: () => void;
  onSelect: (sortMode: ClientTaskSortMode) => void;
}) {
  const menuRef = useRef<HTMLDivElement>(null);
  useFloatingMenu(Boolean(menu), menuRef, onClose, menu);

  if (!menu) return null;
  return createPortal(
    <OcMenuSurface
      aria-label="Task sidebar options"
      data-settings-layer="true"
      className="task-actions-menu fixed z-[100] w-[236px]"
      role="menu"
      style={{ left: menu.left, top: menu.top } as CSSProperties}
      surfaceRef={menuRef}
    >
      <div className="px-2 py-1 text-sm text-token-description-foreground">Sort by</div>
      <OcMenuItem
        aria-checked={sortMode === "recent"}
        label="Last updated"
        onClick={() => onSelect("recent")}
        role="menuitemradio"
        selected={sortMode === "recent"}
      />
      <OcMenuItem
        aria-checked={sortMode === "manual"}
        label="Manual order"
        onClick={() => onSelect("manual")}
        role="menuitemradio"
        selected={sortMode === "manual"}
      />
    </OcMenuSurface>,
    document.body,
  );
}

function ProjectDeleteDialog({
  blockedReason,
  busy,
  open,
  project,
  runtimeBacked,
  onCancel,
  onDelete,
}: {
  blockedReason: string;
  busy: boolean;
  open: boolean;
  project: ClientChatProject | null;
  runtimeBacked: boolean;
  onCancel: () => void;
  onDelete: () => void;
}) {
  const cancelRef = useRef<HTMLButtonElement>(null);
  if (!open || !project) return null;
  return (
    <OcDialog
      ariaLabelledBy="octopus-project-delete-title"
      initialFocusRef={cancelRef}
      onClose={onCancel}
      open={open}
      panelClassName="p-5"
      size="compact"
    >
        <OcDialogHeader className="p-0" title="Delete project?" titleId="octopus-project-delete-title" />
        <p className="mt-2 text-base text-token-description-foreground">
          {runtimeBacked
            ? `“${project.name}” is a real execution environment. Deleting the project removes that EE record; its ledger history remains.`
            : `“${project.name}” will be removed from this local workspace.`}
        </p>
        {blockedReason ? <p className="mt-2 text-sm text-token-error-foreground">{blockedReason}</p> : null}
        <OcDialogFooter className="mt-5 min-h-0 border-0 p-0">
          <OcButton buttonRef={cancelRef} onClick={onCancel} variant="ghost">Cancel</OcButton>
          <OcButton disabled={busy || Boolean(blockedReason)} onClick={onDelete} tone="danger" variant="primary">Delete</OcButton>
        </OcDialogFooter>
    </OcDialog>
  );
}

function ProjectDialog({
  busy,
  creating,
  initialColor,
  initialIcon,
  initialName,
  initialFolders,
  open,
  parent,
  runtimeBacked,
  submitLabel,
  templateOptions,
  title,
  onCancel,
  onSubmit,
}: {
  busy: boolean;
  creating: boolean;
  initialColor: string;
  initialIcon: string;
  initialName: string;
  initialFolders: string[];
  open: boolean;
  parent: ProjectDialogParent | null;
  runtimeBacked: boolean;
  submitLabel: string;
  templateOptions: ProjectPolicyOption[];
  title: string;
  onCancel: () => void;
  onSubmit: (value: ProjectDialogValue) => Promise<void>;
}) {
  const inputRef = useRef<HTMLInputElement>(null);
  const [name, setName] = useState("");
  const [sourceFolders, setSourceFolders] = useState<string[]>([]);
  const [icon, setIcon] = useState<string>(PROJECT_ICON_OPTIONS[0]);
  const [color, setColor] = useState<string>(PROJECT_COLOR_OPTIONS[0]);
  const [templateName, setTemplateName] = useState("");
  const [submissionError, setSubmissionError] = useState("");
  const templateOptionKey = templateOptions.map((option) => option.id).join("\u0001");
  const selectedPolicy = templateOptions.find((option) => option.id === templateName);
  useEffect(() => {
    if (!open) return;
    setName(initialName);
    setSourceFolders(initialFolders);
    setIcon((PROJECT_ICON_OPTIONS as readonly string[]).includes(initialIcon) ? initialIcon : PROJECT_ICON_OPTIONS[0]);
    setColor(initialColor || DEFAULT_PROJECT_COLOR);
    setTemplateName(creating ? templateOptions.find((option) => option.inheritsParentPolicy)?.id
      ?? (templateOptions.length === 1 ? templateOptions[0].id : "") : "");
    setSubmissionError("");
  }, [creating, initialColor, initialIcon, initialName, open, parent?.eeId]);

  useEffect(() => {
    if (!open || !creating) return;
    setTemplateName((current) => templateOptions.some((option) => option.id === current) ? current
      : templateOptions.find((option) => option.inheritsParentPolicy)?.id
        ?? (templateOptions.length === 1 ? templateOptions[0].id : ""));
  }, [creating, open, templateOptionKey]);

  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (busy) return;
    const value = name.trim();
    if (!value) return;
    if (runtimeBacked && creating && (!selectedPolicy || !parent?.eeId)) return;
    setSubmissionError("");
    try {
      await onSubmit({ name: value, icon, color, templateName, sourceFolders });
    } catch (failure) {
      setSubmissionError(failure instanceof Error ? failure.message : String(failure));
    }
  };
  return (
    <OcDialog
      ariaLabelledBy="octopus-project-name-title"
      busy={busy}
      initialFocusRef={inputRef}
      onClose={onCancel}
      open={open}
      size="default"
    >
          <form className="flex flex-col gap-0 px-5 py-5 text-base leading-normal tracking-normal" onSubmit={submit}>
            <OcDialogHeader className="p-0 pb-3" closeDisabled={busy} onClose={onCancel} title={title} titleId="octopus-project-name-title" />
            {runtimeBacked && creating ? (
              <div className="flex w-full flex-col gap-2 pt-3 first:pt-0">
                <span className="text-base text-token-foreground">Parent project</span>
                <div className="flex min-w-0 items-center gap-3 rounded-xl border border-token-border bg-token-main-surface-primary px-3 py-2.5">
                  <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-token-foreground/5 text-token-icon-foreground">
                    <Glyph name="folder" size={16} />
                  </span>
                  <span className="flex min-w-0 flex-1 flex-col">
                    <span className="truncate text-base text-token-foreground">{parent?.displayName || "Unavailable"}</span>
                    <span className="truncate text-sm text-token-description-foreground">
                      {parent ? "This project stays within its parent’s permissions" : "Choose a valid parent before creating this project"}
                    </span>
                  </span>
                </div>
              </div>
            ) : null}
            <div className="flex w-full flex-col gap-2 pt-3 first:pt-0">
              <label className="text-base text-token-foreground" htmlFor="octopus-project-name">Project name</label>
              <div className="flex items-center gap-2">
                <ProjectAppearancePicker
                  disabled={busy}
                  iconOptions={PROJECT_ICON_OPTIONS.map((option) => ({ id: option, label: `Use ${option}`, icon: <Glyph name={option} size={20} /> }))}
                  onColorChange={setColor}
                  onIconChange={setIcon}
                  projectName={name}
                  selectedColor={color}
                  selectedIcon={icon}
                  trigger={<ProjectLogo color={color} icon={icon} size={20} iconSize={20} />}
                />
                <OcTextField
                  className="min-w-0 flex-1"
                  id="octopus-project-name"
                  inputRef={inputRef}
                  label="Project name"
                  onChange={(event) => setName(event.currentTarget.value)}
                  placeholder="Copenhagen Trip"
                  value={name}
                />
              </div>
            </div>
            {runtimeBacked ? <ProjectSourceFolders folders={sourceFolders} onChange={setSourceFolders} disabled={busy} onError={setSubmissionError} /> : null}
            {runtimeBacked && creating ? (
              <div className="flex w-full flex-col gap-2 pt-3 first:pt-0">
                <OcSelect
                  id="octopus-project-template"
                  label="Project policy"
                  onChange={setTemplateName}
                  options={templateOptions.map((option) => ({
                    value: option.id,
                    label: option.label,
                    description: option.description || undefined,
                  }))}
                  placeholder="Choose a policy"
                  showLabel
                  stretch
                  value={templateName}
                />
                {selectedPolicy ? (
                  <details className="text-sm leading-5 text-token-description-foreground">
                    <summary className="cursor-pointer">Policy source and limits</summary>
                    <p className="mt-2 break-words">Source: {selectedPolicy.source}</p>
                    <p>{selectedPolicy.inheritsParentPolicy
                      ? "Access follows the parent’s policy through the existing Project template. Nested projects use that same template."
                      : selectedPolicy.goal}</p>
                    <p>All parent restrictions still apply. Creation is checked against the formed child policy.</p>
                  </details>
                ) : <p className="text-sm leading-5 text-token-description-foreground">Choose an installed policy allowed by the parent.</p>}
              </div>
            ) : null}
            <div className="flex w-full flex-col pt-3 first:pt-0">
              <aside className="flex items-center rounded-xl bg-token-foreground/5 p-3">
                <Glyph className="icon-base mr-3 shrink-0 text-token-description-foreground" name="lightbulb" size={21} />
                <p className="text-sm text-pretty text-token-description-foreground">
                  {runtimeBacked && creating
                    ? `Tasks in this project stay within ${parent?.displayName || "the parent"}’s permissions. A preset can restrict them further; it never grants extra access.`
                    : "Projects group tasks around one goal. Select a project to focus its tasks; select it again to see everything."}
                </p>
              </aside>
            </div>
            {submissionError ? <p className="mt-3 rounded-xl bg-token-error-background/20 px-3 py-2 text-sm text-token-error-foreground" role="alert">{submissionError}</p> : null}
            <OcDialogFooter className="min-h-0 border-0 px-0 pb-0 pt-3">
                <OcButton
                  disabled={busy || !name.trim() || (runtimeBacked && creating && (!selectedPolicy || !parent?.eeId))}
                  type="submit"
                  variant="primary"
                >
                  {busy ? creating ? "Creating…" : "Saving…" : submitLabel}
                </OcButton>
            </OcDialogFooter>
          </form>
    </OcDialog>
  );
}

function TaskListState({
  empty,
  emptyLabel,
  error,
  loadingLabel,
  onRetry,
  phase,
}: {
  empty: boolean;
  emptyLabel: string;
  error: string;
  loadingLabel: string;
  onRetry: () => Promise<void>;
  phase: ProjectionResourcePhase;
}) {
  const [retrying, setRetrying] = useState(false);
  if (phase === "initial-loading") return empty ? <OcLoadingRows label={loadingLabel} /> : null;
  return (
    <>
      {empty && phase !== "failed" ? <div className="p-2 text-base text-token-description-foreground opacity-50">{emptyLabel}</div> : null}
      {phase === "failed" || phase === "degraded" ? (
        <ProjectionResourceIssue
          className="mt-1"
          detail={error}
          onRetry={() => {
            setRetrying(true);
            void onRetry().finally(() => setRetrying(false));
          }}
          retrying={retrying}
          title={phase === "failed" ? "Tasks could not be loaded" : "Tasks could not be refreshed"}
        />
      ) : null}
    </>
  );
}

function FadeLabel({ className, text }: { className?: string; text: string }) {
  const ref = useRef<HTMLSpanElement>(null);
  const [overflowing, setOverflowing] = useState(false);

  useLayoutEffect(() => {
    const node = ref.current;
    if (!node) return;
    const measure = () => setOverflowing(node.scrollWidth > node.clientWidth + 1);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    return () => observer.disconnect();
  }, [text]);

  return (
    <span className={`${overflowing ? "text-fade-truncate" : "block w-full truncate"}${className ? ` ${className}` : ""}`} ref={ref}>
      {text}
    </span>
  );
}

function SidebarSectionTitle(props: {
  collapsed: boolean;
  label: string;
  onClick: () => void;
  onOptions?: (anchor: HTMLElement) => void;
}) {
  return (
    <div className="flex min-w-0 flex-1">
      <SectionToggle {...props} />
    </div>
  );
}

function SectionToggle({
  collapsed,
  label,
  onClick,
  onOptions,
}: {
  collapsed: boolean;
  label: string;
  onClick: () => void;
  onOptions?: (anchor: HTMLElement) => void;
}) {
  return (
    <button
      aria-expanded={!collapsed}
      className="group/section-toggle flex min-w-0 flex-1 cursor-interaction items-center gap-1 rounded-md py-0.5 pr-1 text-left focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2"
      onClick={onClick}
      onContextMenu={onOptions ? (event) => { event.preventDefault(); onOptions(event.currentTarget); } : undefined}
      onKeyDown={onOptions ? (event) => {
        if (event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10")) return;
        event.preventDefault();
        onOptions(event.currentTarget);
      } : undefined}
      type="button"
    >
      <span className="min-w-0 truncate">{label}</span>
      <Glyph
        aria-hidden="true"
        className={`icon-2xs shrink-0 text-token-foreground/50 transition-transform group-hover/section-toggle:opacity-100 group-focus-visible/section-toggle:opacity-100 hover:text-token-foreground focus-visible:text-token-foreground ${collapsed ? "opacity-100 -rotate-90" : "opacity-0 rotate-0"}`}
        name="chevronDown"
        size={12}
      />
    </button>
  );
}


function surfaceGlyph(surface: ClientSurfaceDescriptor) {
  return surface.glyph;
}

function mergeVisiblePinnedOrder(
  stored: string[],
  visibleIds: Set<string>,
  visibleOrder: string[],
): string[] {
  const hidden = stored.filter((id) => !visibleIds.has(id));
  return [...visibleOrder, ...hidden.filter((id) => !visibleOrder.includes(id))];
}

function dragData(value: unknown): SidebarDragData | null {
  if (!value || typeof value !== "object") return null;
  const data = value as Partial<SidebarDragData>;
  if (
    (data.entity !== "surface" && data.entity !== "chat" && data.entity !== "project")
    || typeof data.itemId !== "string"
    || typeof data.containerId !== "string"
  ) return null;
  return data as SidebarDragData;
}

type SidebarDragTarget = {
  containerId: string;
  entity?: SidebarDragData["entity"];
  itemId?: string;
};

function dragTarget(event: DragEndEvent): SidebarDragTarget | null {
  const value = event.over?.data.current;
  if (!value || typeof value !== "object") return null;
  const data = value as Partial<SidebarDragData>;
  if (typeof data.containerId !== "string") return null;
  return {
    containerId: data.containerId,
    entity: data.entity,
    itemId: data.itemId,
  };
}

function moveBefore(values: string[], item: string, before: string | null): string[] {
  const next = values.filter((value) => value !== item);
  const index = before ? next.indexOf(before) : -1;
  next.splice(index >= 0 ? index : next.length, 0, item);
  return next;
}

function orderChats(
  chats: ChatRun[],
  orderIds: string[],
  sortMode: ClientTaskSortMode,
): ChatRun[] {
  if (sortMode === "manual") {
    const manualRank = new Map(orderIds.map((runId, index) => [runId, index]));
    const rankOf = (chat: ChatRun) => manualRank.get(chat.runId) ?? Number.MAX_SAFE_INTEGER;
    return [...chats].sort((left, right) => rankOf(left) - rankOf(right));
  }
  return [...chats].sort((left, right) => right.updatedAtMs - left.updatedAtMs);
}

function orderProjects(projects: ClientChatProject[], orderIds: string[]): ClientChatProject[] {
  const projectById = new Map(projects.map((project) => [project.id, project]));
  const ordered = orderIds.flatMap((projectId) => {
    const project = projectById.get(projectId);
    if (!project) return [];
    projectById.delete(projectId);
    return [project];
  });
  return [...ordered, ...projectById.values()];
}

function taskSubtitle(chat: ChatRun): string {
  const messages = `${chat.messageCount} ${chat.messageCount === 1 ? "message" : "messages"}`;
  const lineage = chat.writable ? chat.continuedFrom ? "continued run" : "run" : "read-only run";
  return `${lineage}, ${messages}${chat.providerInterfaceId ? `, ${chat.providerInterfaceId}` : ""}`;
}

function scheduledLabelForChat(chat: ChatRun, now: number): string {
  if (chat.turnState !== "idle" || !chat.nextScheduledAtMs || chat.nextScheduledAtMs <= now) return "";
  const at = new Date(chat.nextScheduledAtMs);
  const time = at.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  const sameDay = at.toDateString() === new Date(now).toDateString();
  return `Scheduled · ${sameDay ? time : `${at.toLocaleDateString([], { day: "numeric", month: "short" })} ${time}`}`;
}

function useSidebarClock(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const update = () => setNow(Date.now());
    const interval = window.setInterval(update, 30_000);
    document.addEventListener("visibilitychange", update);
    return () => {
      window.clearInterval(interval);
      document.removeEventListener("visibilitychange", update);
    };
  }, []);
  return now;
}

function moveSidebarFocus(event: KeyboardEvent<HTMLElement>): void {
  if (event.defaultPrevented || event.ctrlKey || event.metaKey || event.altKey || event.shiftKey) return;
  if (event.key !== "ArrowDown" && event.key !== "ArrowUp" && event.key !== "Home" && event.key !== "End") return;
  const target = event.target;
  if (!(target instanceof HTMLElement) || !target.closest("[data-sidebar-nav-item]")) return;
  const rows = [...event.currentTarget.querySelectorAll<HTMLElement>("[data-sidebar-nav-item]")]
    .filter((row) => row.offsetParent !== null && !row.hasAttribute("disabled"));
  if (!rows.length) return;
  const current = rows.findIndex((row) => row === target || row.contains(target));
  const next = event.key === "Home"
    ? 0
    : event.key === "End"
      ? rows.length - 1
      : event.key === "ArrowDown"
        ? (current + 1 + rows.length) % rows.length
        : (current - 1 + rows.length) % rows.length;
  event.preventDefault();
  rows[next]?.focus();
}

function relativeTime(ms: number, now = Date.now()): string {
  if (!ms) return "";
  const delta = Math.max(0, now - ms);
  const minutes = Math.floor(delta / 60_000);
  if (minutes < 1) return "now";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  return `${Math.floor(hours / 24)}d`;
}
