module.exports = { mutateOctopusOsWorkspace };

const path = require("node:path");
const { createOctopusOsAgentTemplate, mutateOctopusOsAgentTemplate, requiredAgentName } = require("./agentTemplates.cjs");
const { configureOctopusOsApplication, configureOctopusOsDriver, configureOctopusOsProjectDefaults } = require("./applicationConfiguration.cjs");
const { readRegularJsonDocument } = require("./jsonFiles.cjs");
const { mutateLocalPackageInventory, requireOctopusOsScope, workspaceProjectMutationScope } = require("./localPackageInventory.cjs");
const { parseOctopusOsSidecar } = require("./sidecars.cjs");
const { configureOctopusOsTaskApplication } = require("./taskApplications.cjs");
const { setOctopusOsTaskArchived } = require("./taskArchive.cjs");
const { deleteOctopusOsTask } = require("./taskDeletion.cjs");
const { exactObjectKeys, requiredSafeRegistryId, stringValue } = require("./values.cjs");

let osWorkspaceMutationTail = Promise.resolve();

async function mutateOctopusOsWorkspace(registryRoot, octopusHomeRoot, scope, request, hooks = {}) {
  const action = stringValue(request?.action);
  if (![
    "create_project",
    "create_agent_template",
    "add_agent_preset",
    "configure_agent_template",
    "delete_agent_template",
    "delete_project",
    "update_project_presentation",
    "delete_task",
    "rename_task",
    "set_task_archived",
    "install_application",
    "remove_application",
    "install_package",
    "remove_package",
    "repair_package",
    "apply_application_pack",
    "install_driver",
    "remove_driver",
    "configure_application",
    "configure_driver",
    "configure_project_defaults",
    "configure_task_application",
  ].includes(action)) {
    throw new Error("Octopus OS workspace mutation action is invalid");
  }
  const execute = async () => {
    if ([
      "install_application",
      "remove_application",
      "install_package",
      "remove_package",
      "repair_package",
      "apply_application_pack",
      "install_driver",
      "remove_driver",
    ].includes(action)) {
      return mutateLocalPackageInventory(
        registryRoot,
        octopusHomeRoot,
        scope,
        request,
        hooks,
      );
    }
    const snapshot = requireOctopusOsScope(scope);
    if (action === "set_task_archived") {
      return setOctopusOsTaskArchived(registryRoot, octopusHomeRoot, snapshot, request, hooks);
    }
    if (action === "rename_task") {
      const runId = requiredSafeRegistryId(request.runId, "runId");
      const name = requiredAgentName(request.name);
      const task = snapshot.clientRuns.find((row) => row.run_id === runId && row.surface_role === "task");
      if (!task) throw new Error(`Task '${runId}' is not an editable task in this Home.`);
      if (typeof hooks.commitClientAuthoring !== "function") throw new Error("Client authoring transaction is unavailable.");
      const interfaceId = requiredSafeRegistryId(task.interface_id, "task client interface");
      const file = path.join(registryRoot, "interface", interfaceId, "octopus_os.json");
      const current = await readRegularJsonDocument(file, "task presentation");
      parseOctopusOsSidecar(current.document, interfaceId, file);
      if (current.document.component !== "client" || current.document.surface_role !== "task"
        || current.document.owner_interface !== interfaceId) {
        throw new Error("Task presentation no longer belongs to the selected run.");
      }
      const next = { ...current.document, presentation: { name, projects: [] } };
      parseOctopusOsSidecar(next, interfaceId, file);
      await hooks.commitClientAuthoring({
        label: "rename Octopus task",
        writes: [{ file, beforeDigest: current.digest, next }],
        removals: [], publishBarrierPath: file,
      });
      return { action, runId, name };
    }
    if (action === "add_agent_preset") {
      exactObjectKeys(request, ["action", "operationId", "home", "workspace", "projectEeId", "packId", "expectedPackManifestDigest", "presetId", "providerInterfaceId", "providerModel", "reasoningEffort", "serviceTier"], ["action", "operationId", "home", "workspace", "projectEeId", "packId", "expectedPackManifestDigest", "presetId", "providerInterfaceId", "providerModel", "reasoningEffort"], "Agent preset request");
      if (request.home !== octopusHomeRoot || typeof hooks.withAgentPreset !== "function") throw new Error("Agent preset authoring is unavailable for this Home");
      return hooks.withAgentPreset(request, async (preset) => {
        await preset.verify();
        const result = await createOctopusOsAgentTemplate(registryRoot, octopusHomeRoot, snapshot, {
          ...request,
          sourceWorkerInterfaceId: preset.sourceInterface.id,
          name: preset.recipe.name,
          icon: preset.recipe.icon,
        }, hooks.commitClientAuthoring, null, preset);
        return { ...result, action: "add_agent_preset" };
      });
    }
    if (action === "create_agent_template") {
      return createOctopusOsAgentTemplate(
        registryRoot,
        octopusHomeRoot,
        snapshot,
        request,
        hooks.commitClientAuthoring,
        hooks.chatContexts,
      );
    }
    if (action === "configure_agent_template" || action === "delete_agent_template") {
      return mutateOctopusOsAgentTemplate(
        registryRoot,
        octopusHomeRoot,
        snapshot,
        request,
        hooks.commitClientAuthoring,
        hooks.chatContexts,
      );
    }
    if (["create_project", "delete_project", "update_project_presentation"].includes(action)) {
      if (typeof hooks.mutateWorkspaceProject !== "function") {
        throw new Error("Workspace Project authoring transaction is unavailable");
      }
      return hooks.mutateWorkspaceProject(workspaceProjectMutationScope(snapshot), request);
    }
    if (action === "delete_task") {
      const remove = () => deleteOctopusOsTask(
        registryRoot,
        octopusHomeRoot,
        snapshot,
        request,
        hooks.releaseTaskWorkerEnvironments,
        hooks.commitClientAuthoring,
        hooks,
      );
      return hooks.taskLifecycle ? hooks.taskLifecycle.withTaskLifecycleLock(request.runId, remove) : remove();
    }
    if (action === "configure_application") {
      return configureOctopusOsApplication(
        registryRoot,
        octopusHomeRoot,
        snapshot,
        request,
        hooks.commitClientAuthoring,
      );
    }
    if (action === "configure_driver") {
      return configureOctopusOsDriver(
        registryRoot,
        octopusHomeRoot,
        snapshot,
        request,
        hooks.commitClientAuthoring,
      );
    }
    if (action === "configure_project_defaults") {
      return configureOctopusOsProjectDefaults(
        registryRoot,
        octopusHomeRoot,
        snapshot,
        request,
        hooks.commitClientAuthoring,
      );
    }
    return configureOctopusOsTaskApplication(
      registryRoot,
      octopusHomeRoot,
      snapshot,
      request,
      hooks.withTaskAuthoringLock,
      hooks.planTaskApplicationAmendment,
      hooks.commitClientAuthoring,
    );
  };
  const previous = osWorkspaceMutationTail;
  let release;
  const current = new Promise((resolve) => { release = resolve; });
  osWorkspaceMutationTail = previous.then(() => current);
  await previous;
  try {
    return await execute();
  } finally {
    release();
  }
}
