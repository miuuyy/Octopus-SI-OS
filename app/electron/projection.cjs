const path = require("node:path");
const { DESCRIPTOR_REGISTRIES } = require("./projection/constants.cjs");
const { nativeTurnRecordedInputs } = require("./projection/operatorChatEvents.cjs");
const { mutateApplicationPrivateConfiguration, readApplicationLogoAsset } = require("./projection/privateConfiguration.cjs");
const { readProviderInterfaceDescriptors } = require("./projection/providerDescriptors.cjs");
const { armSetupTriggers, mutateScheduledInterface, setupTriggerEntries, setupTriggerProof, setupTriggerTaskPlanner } = require("./projection/scheduled.cjs");
const { parseApplicationManifest } = require("./projection/sidecars.cjs");
const { arrayValue, numberValue, objectOrEmpty, requiredString, stringValue, topologyArtifactRow } = require("./projection/values.cjs");
const { mutateOctopusOsWorkspace } = require("./projection/workspaceMutations.cjs");
const { invalidationProjection } = require("./projection/invalidation.cjs");
const { queryRouterProjection } = require("./projection/queryRouter.cjs");
const { movementPagesProjection } = require("./projection/movementPages.cjs");
const { clientSurfacesProjection } = require("./projection/clientSurfaces.cjs");
const { interfaceArtifactsProjection } = require("./projection/interfaceArtifacts.cjs");
const { runActivityProjection } = require("./projection/runActivity.cjs");
const { configurationProjection } = require("./projection/configuration.cjs");
const { workerEnvironmentsProjection } = require("./projection/workerEnvironments.cjs");
const { descriptorRowsProjection } = require("./projection/descriptorRows.cjs");
const { ledgerProjection } = require("./projection/ledger.cjs");
const { operatorChatIndexProjection } = require("./projection/operatorChatIndex.cjs");
const { operatorOsProjection } = require("./projection/operatorOs.cjs");
const { operatorChatContextProjection } = require("./projection/operatorChatContext.cjs");
const { operatorChatTurnsProjection } = require("./projection/operatorChatTurns.cjs");
const { operatorChatListProjection } = require("./projection/operatorChatList.cjs");
const { operatorChatMessagesProjection } = require("./projection/operatorChatMessages.cjs");
const { scheduledInterfacesProjection } = require("./projection/scheduledInterfaces.cjs");
const { topologyRowsProjection } = require("./projection/topologyRows.cjs");

function createProjectionContext(options) {
  const resolveRepoRoot = options.resolveRepoRoot;
  const resolveSourceRegistry = typeof options.sourceRegistry === "function"
    ? options.sourceRegistry
    : () => path.join(resolveRepoRoot(), "registry");
  const resolveLocalPackageRegistry = typeof options.localPackageRegistry === "function"
    ? options.localPackageRegistry
    : resolveSourceRegistry;
  const resolveLocalPackageLibrary = typeof options.localPackageLibrary === "function"
    ? options.localPackageLibrary
    : () => path.join(resolveRepoRoot(), "app", "distribution", "local-packages");
  const octopusHome = options.octopusHome;
  const onChange = typeof options.onChange === "function" ? options.onChange : () => {};
  const beforeRead = typeof options.beforeRead === "function" ? options.beforeRead : async () => {};
  const nativeBrowserTargetsForProjection = typeof options.nativeBrowserTargetsForProjection === "function"
    ? options.nativeBrowserTargetsForProjection : null;
  const nativeBrowserDownloadsForProjection = typeof options.nativeBrowserDownloadsForProjection === "function"
    ? options.nativeBrowserDownloadsForProjection : null;
  const runtimeLaunchEnvironment = typeof options.runtimeLaunchEnvironment === "function"
    ? options.runtimeLaunchEnvironment
    : () => null;
  // The native publication boundary that recovers an interrupted package transaction found during
  // a catalog read. A store without one can still read a Home whose package state is quiet.
  const localPackagePublication = typeof options.localPackagePublication === "function"
    ? options.localPackagePublication
    : () => undefined;
  const store = {
    options,
    resolveRepoRoot,
    resolveSourceRegistry,
    resolveLocalPackageRegistry,
    resolveLocalPackageLibrary,
    octopusHome,
    onChange,
    beforeRead,
    nativeBrowserTargetsForProjection,
    nativeBrowserDownloadsForProjection,
    runtimeLaunchEnvironment,
    localPackagePublication,
  };
  for (const projection of [invalidationProjection, queryRouterProjection, movementPagesProjection, clientSurfacesProjection, interfaceArtifactsProjection, runActivityProjection, configurationProjection, workerEnvironmentsProjection, descriptorRowsProjection, ledgerProjection, operatorChatIndexProjection, operatorOsProjection, operatorChatContextProjection, operatorChatTurnsProjection, operatorChatListProjection, operatorChatMessagesProjection, scheduledInterfacesProjection, topologyRowsProjection]) {
    Object.assign(store, projection(store));
  }
  store.close = () => {
    store.closeProjectionWatchers();
    store.closeConfigurationProjection();
    store.closeLedgerProjection();
  };
  return store;
}

function createProjectionStore(options) {
  const store = createProjectionContext(options);
  return {
    read: store.readProjection,
    generation: () => store.projectionGeneration,
    invalidate: store.invalidateProjection,
    ensureWatchers: store.ensureProjectionWatchers,
    close: store.close,
  };
}

module.exports = {
  createProjectionContext,
  nativeTurnRecordedInputs,
  topologyArtifactRow,
  createProjectionStore,
  mutateApplicationPrivateConfiguration,
  mutateOctopusOsWorkspace,
  mutateScheduledInterface,
  setupTriggerEntries,
  setupTriggerProof,
  armSetupTriggers,
  setupTriggerTaskPlanner,
  readProviderInterfaceDescriptors,
  parseApplicationManifest,
  readApplicationLogoAsset,
  DESCRIPTOR_REGISTRIES,
  stringValue,
  arrayValue,
  requiredString,
  numberValue,
  objectOrEmpty,
};
