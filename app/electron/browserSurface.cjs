// Presentation adapter only. The browser kind owns engine creation and tool authority.
function createNativeBrowserSurfaceAdapter({ getHost, getHome, requireHome, resolveSurface, resolveEnvironments, windowForSender, requireAccess, reportError, chooseDownloadDirectory }) {
  const attachments = new Map();
  const formations = new Map();
  let revalidation = null;
  let revalidateAgain = false;

  function record(value, keys) {
    if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).some((key) => !keys.includes(key))) throw new Error("Invalid native browser presentation request.");
  }
  function identity(value) {
    if (typeof value !== "string" || !value || value.length > 1024 || value.trim() !== value || /[\x00-\x1f]/.test(value)) throw new Error("Invalid native browser presentation identity.");
    return value;
  }
  function integer(value, minimum = 1, maximum = Number.MAX_SAFE_INTEGER) {
    if (!Number.isSafeInteger(value) || value < minimum || value > maximum) throw new Error("Invalid native browser presentation revision.");
  }
  function epochs(request) {
    identity(request.hostGeneration); integer(request.mountGeneration, 1, 2147483647);
  }
  function projection(workerEnvironmentId, surface) {
    if (surface?.kind !== "browser" || surface.mode !== "direct" || surface.protocol !== "browser.native.v1") throw new Error("This surface does not declare a native browser target.");
    const data = surface.data;
    if (!data || data.formation_pin !== surface.formationPin) throw new Error("Native browser formation is not current.");
    return {
      protocol: "browser.native.v1", worker_environment_id: identity(workerEnvironmentId),
      formation_pin: identity(surface.formationPin), engine_handle: identity(data.engine_handle),
      host_generation: identity(data.host_generation), target_id: identity(data.target_id),
      browser_context_id: identity(data.browser_context_id),
    };
  }
  function owned(token, sender, request) {
    requireAccess(token); epochs(request);
    const entry = attachments.get(identity(request.attachmentId));
    if (!entry || entry.token !== token || entry.sender !== sender || entry.cancelled || sender.isDestroyed()
      || entry.hostGeneration !== request.hostGeneration || entry.generation !== request.mountGeneration) throw new Error("Native browser presentation is unavailable to this window or generation.");
    requireHome(entry.home);
    return entry;
  }
  function checkState(entry, state) {
    if (!state || state.attachmentId !== entry.id || state.hostGeneration !== entry.hostGeneration || state.mountGeneration !== entry.generation
      || state.protocol !== "browser.native.v1" || state.presentationProtocol !== entry.presentationProtocol
      || state.workerEnvironmentId !== entry.workerEnvironmentId || state.surfaceId !== entry.surfaceId
      || state.targetId !== entry.projection.target_id || state.engineHandle !== entry.projection.engine_handle
      || state.browserContextId !== entry.projection.browser_context_id || !["prepared", "attached", "ready"].includes(state.presentationPhase)) throw new Error("Native browser presentation returned a different target or generation.");
    integer(state.viewportSequence, 0); integer(state.modelViewport?.revision, 0);
    if (typeof state.modelActive !== "boolean" || typeof state.modelTurnKey !== "string") throw new Error("Native browser turn presentation is invalid.");
    if (state.cursor !== null) {
      record(state.cursor, ["moveSequence", "x", "y", "metricsRevision", "sourceMovementSeq"]);
      integer(state.cursor.moveSequence); integer(state.cursor.metricsRevision, 0); integer(state.cursor.sourceMovementSeq);
      if (!Number.isFinite(state.cursor.x) || !Number.isFinite(state.cursor.y) || state.cursor.metricsRevision !== state.modelViewport.revision) throw new Error("Native browser cursor point is invalid.");
    }
    const size = state.modelViewport.emulatedSize;
    if (size !== null && (!size || !Number.isFinite(size.width) || !Number.isFinite(size.height) || size.width <= 0 || size.height <= 0)) throw new Error("Native browser model viewport is invalid.");
    return state;
  }
  function detach(entry) {
    entry.cancelled = true;
    if (attachments.get(entry.id) === entry) attachments.delete(entry.id);
    entry.sender.removeListener("destroyed", entry.onDestroyed);
    entry.sender.removeListener("render-process-gone", entry.onGone);
    entry.sender.removeListener("did-start-navigation", entry.onNavigation);
    if (entry.prepared) {
      entry.prepared = false;
      entry.host.releasePresentation({ attachmentId: entry.id, sender: entry.sender, hostGeneration: entry.hostGeneration, mountGeneration: entry.generation });
    }
  }
  async function closeEntry(entry) { detach(entry); await entry.opening; detach(entry); }
  async function current(entry) {
    try {
      requireHome(entry.home);
      const surface = await resolveSurface(entry.workerEnvironmentId, entry.surfaceId);
      requireHome(entry.home);
      if (attachments.get(entry.id) !== entry || entry.cancelled || entry.sender.isDestroyed()
        || JSON.stringify(projection(entry.workerEnvironmentId, surface)) !== JSON.stringify(entry.projection)) throw new Error("Native browser target or formation changed.");
      requireAccess(entry.token);
      if (getHost() !== entry.host) throw new Error("Native browser engine host changed.");
      entry.host.getTarget(entry.projection);
      return surface;
    } catch (error) {
      if (attachments.get(entry.id) === entry) {
        if (!entry.sender.isDestroyed()) entry.sender.send("octopus:native-browser-viewer-event", {
          type: "error", attachmentId: entry.id, hostGeneration: entry.hostGeneration, mountGeneration: entry.generation,
          error: { code: "presentation_unavailable", message: error.message },
        });
        detach(entry);
      }
      throw error;
    }
  }
  function failed(entry, error) {
    if (!entry.sender.isDestroyed()) entry.sender.send("octopus:native-browser-viewer-event", {
      type: "error", attachmentId: entry.id, hostGeneration: entry.hostGeneration, mountGeneration: entry.generation,
      error: { code: "presentation_unavailable", message: error.message },
    });
    return closeEntry(entry).catch(reportError);
  }
  function packet(request) {
    record(request, ["attachmentId", "hostGeneration", "mountGeneration", "viewportSequence", "metricsRevision", "stageBounds", "pageBounds", "visualBounds", "scale", "visible"]);
    epochs(request); integer(request.viewportSequence); integer(request.metricsRevision, 0);
    for (const name of ["stageBounds", "pageBounds", "visualBounds"]) {
      record(request[name], ["x", "y", "width", "height"]);
      if (["x", "y", "width", "height"].some((key) => !Number.isFinite(request[name][key])) || request[name].width < 0 || request[name].height < 0) throw new Error("Invalid native browser presentation geometry.");
    }
    if (!Number.isFinite(request.scale) || request.scale <= 0 || typeof request.visible !== "boolean") throw new Error("Invalid native browser presentation scale or visibility.");
  }
  async function layout(token, sender, request, method) {
    packet(request);
    const entry = owned(token, sender, request);
    if (!entry.prepared) throw new Error("Native browser presentation is not prepared.");
    const pending = entry.layout.then(async () => {
      await current(entry);
      const state = await entry.host[method]({ ...request, sender, projection: entry.projection });
      await current(entry);
      checkState(entry, state);
      entry.state = state;
      return state;
    });
    entry.layout = pending.then(() => {}, () => {});
    return pending;
  }
  const api = {
    async permissions(token, sender, request) {
      record(request, request?.operation === "list" ? ["operation"] : ["operation", "home", "hostGeneration", "id"]);
      if (!["list", "remove"].includes(request.operation)) throw new Error("Unsupported saved browser permission action.");
      const home = getHome(), host = getHost(), win = windowForSender(sender);
      const current = () => {
        requireAccess(token); requireHome(home);
        if (!win || win.isDestroyed() || sender.isDestroyed() || win.webContents !== sender || getHost() !== host) throw new Error("Browser settings lost their exact live client window or host.");
        if (request.operation === "remove" && request.home !== home) throw new Error("Saved browser permission belongs to another Home.");
      };
      current();
      const result = await host.savedPermissions(request.operation === "list" ? { operation: "list" }
        : { operation: "remove", hostGeneration: request.hostGeneration, id: request.id }, current);
      current(); return { home, ...result };
    },
    async prepare(token, sender, request) {
      requireAccess(token);
      record(request, ["attachmentId", "workerEnvironmentId", "surfaceId", "frameToken"]);
      const id = identity(request.attachmentId), home = getHome();
      requireHome(home); identity(request.frameToken);
      if (attachments.has(id)) throw new Error("Native browser presentation already exists.");
      const window = windowForSender(sender);
      if (!window || window.isDestroyed() || sender.isDestroyed() || window.webContents !== sender) throw new Error("Native browser viewing requires its owning client window.");
      const entry = { id, token, sender, window, home, workerEnvironmentId: identity(request.workerEnvironmentId), surfaceId: identity(request.surfaceId), projection: null, generation: 0, hostGeneration: "", host: null, state: null, cancelled: false, prepared: false, opening: null, layout: Promise.resolve() };
      entry.onDestroyed = () => { void closeEntry(entry).catch(reportError); };
      entry.onGone = entry.onDestroyed;
      entry.onNavigation = (_event, _url, isInPlace, isMainFrame) => { if (isMainFrame && !isInPlace) entry.onDestroyed(); };
      attachments.set(id, entry);
      sender.once("destroyed", entry.onDestroyed); sender.on("render-process-gone", entry.onGone); sender.on("did-start-navigation", entry.onNavigation);
      const pending = (async () => {
        const surface = await resolveSurface(entry.workerEnvironmentId, entry.surfaceId);
        requireHome(home);
        entry.projection = projection(entry.workerEnvironmentId, surface);
        if (entry.cancelled || sender.isDestroyed()) throw new Error("Native browser presentation was cancelled before preparation.");
        requireAccess(token);
        const host = getHost(); entry.host = host; host.getTarget(entry.projection);
        const result = await host.preparePresentation({ attachmentId: id, projection: entry.projection, window, sender, frameToken: request.frameToken, surfaceId: entry.surfaceId });
        epochs(result);
        entry.presentationProtocol = result.state?.presentationProtocol;
        if (entry.presentationProtocol === "electron.ownedGuest.v1") identity(result.adoptionToken);
        else if (entry.presentationProtocol !== "electron.webContentsView.v1" || result.adoptionToken !== null
          || result.state?.engine?.mode !== "standard" || result.state?.engine?.capabilities?.nativeView !== true) throw new Error("Browser presentation does not declare a supported engine contract.");
        entry.generation = result.mountGeneration; entry.hostGeneration = result.hostGeneration; entry.prepared = true;
        if (result.hostGeneration !== entry.projection.host_generation) throw new Error("Native browser host generation changed during preparation.");
        if (entry.cancelled) { detach(entry); throw new Error("Native browser presentation was cancelled during preparation."); }
        await current(entry);
        entry.state = checkState(entry, result.state);
        return result;
      })();
      entry.opening = pending.then(() => {}, () => {});
      try { return await pending; } catch (error) { detach(entry); throw error; }
    },
    validatePreparedGuest(sender, guest) {
      for (const entry of attachments.values()) {
        if (entry.sender !== sender || !entry.prepared || entry.cancelled || sender.isDestroyed()) continue;
        try {
          requireAccess(entry.token); requireHome(entry.home);
          if (getHost() !== entry.host) continue;
          entry.host.getTarget(entry.projection);
          if (entry.host.validatePreparedGuest({ sender, guest })) return true;
        } catch { return false; }
      }
      return false;
    },
    acknowledge(token, sender, request) { return layout(token, sender, request, "acknowledgePresentation"); },
    sync(token, sender, request) { return layout(token, sender, request, "syncPresentation"); },
    async cursorArrived(token, sender, request) {
      record(request, ["attachmentId", "hostGeneration", "mountGeneration", "moveSequence", "metricsRevision"]);
      integer(request.moveSequence); integer(request.metricsRevision, 0);
      const entry = owned(token, sender, request);
      await current(entry);
      return entry.host.cursorArrived({ ...request, sender, projection: entry.projection });
    },
    async action(token, sender, request) {
      record(request, ["attachmentId", "hostGeneration", "mountGeneration", "action", "params"]);
      if (!["navigate", "reload", "back", "forward", "stop", "focus", "zoom", "find"].includes(request.action)) throw new Error("Unsupported native browser presentation action.");
      const entry = owned(token, sender, request);
      const surface = await current(entry);
      if (entry.state?.presentationPhase !== "ready") throw new Error("Native browser presentation has not acknowledged its viewport.");
      if (surface.readOnly && request.action !== "find") throw new Error("This native browser surface is read-only.");
      const state = await entry.host.viewerAction({ attachmentId: entry.id, sender, hostGeneration: entry.hostGeneration, mountGeneration: entry.generation, action: request.action, params: request.params === undefined ? {} : request.params });
      await current(entry); entry.state = checkState(entry, state); return state;
    },
    async close(token, sender, request) {
      requireAccess(token); record(request, ["attachmentId", "hostGeneration", "mountGeneration"]); identity(request.attachmentId); epochs(request);
      const entry = attachments.get(request.attachmentId);
      if (!entry || entry.hostGeneration !== request.hostGeneration || entry.generation !== request.mountGeneration) return;
      if (entry.token !== token || entry.sender !== sender) throw new Error("Native browser presentation belongs to another window.");
      await closeEntry(entry);
    },
    async downloads(token, sender, request) {
      requireAccess(token);
      record(request, ["workerEnvironmentId", "surfaceId", "hostGeneration", "action", "id", "promptForLocation", "query"]);
      const home = getHome(); requireHome(home);
      const workerEnvironmentId = identity(request.workerEnvironmentId), surfaceId = identity(request.surfaceId);
      const hostGeneration = identity(request.hostGeneration), window = windowForSender(sender);
      if (!window || window.isDestroyed() || sender.isDestroyed() || window.webContents !== sender) throw new Error("Native downloads require their owning client window.");
      const read = async () => {
        const surface = await resolveSurface(workerEnvironmentId, surfaceId);
        requireAccess(token); requireHome(home);
        if (sender.isDestroyed() || window.isDestroyed() || window.webContents !== sender || surface.mode !== "direct"
          || !["browser.native.v1", "browser.native.downloads.v1"].includes(surface.protocol)
          || surface.data?.formation_pin !== surface.formationPin || surface.data.host_generation !== hostGeneration) throw new Error("Native downloads belong to another room or generation.");
        return { protocol: "browser.native.downloads.v1", worker_environment_id: workerEnvironmentId, formation_pin: identity(surface.formationPin), host_generation: hostGeneration };
      };
      const room = await read(), host = getHost();
      const validateCurrent = async () => {
        if (host !== getHost() || JSON.stringify(await read()) !== JSON.stringify(room)) throw new Error("Native download source changed during the request.");
      };
      let snapshot;
      const action = identity(request.action);
      if (action === "search") {
        record(request.query, ["text", "offset"]);
        if (typeof request.query.text !== "string" || Buffer.byteLength(request.query.text) > 16 * 1024 * 1024
          || !Number.isSafeInteger(request.query.offset) || request.query.offset < 0 || request.query.offset > 2147483547 || request.id !== undefined || request.promptForLocation !== undefined) throw new Error("Invalid native download history query.");
      } else if (request.query !== undefined) throw new Error("This download action does not take a history query.");
      if (["choose-directory", "reset-directory", "set-prompt"].includes(action)) {
        if (request.id !== undefined || (action === "set-prompt" ? typeof request.promptForLocation !== "boolean" : request.promptForLocation !== undefined)) throw new Error("Invalid native download preference action.");
        let changes;
        if (action === "choose-directory") {
          if (typeof chooseDownloadDirectory !== "function") throw new Error("Native download folder picker is unavailable.");
          const directory = await chooseDownloadDirectory(window);
          if (host !== getHost() || JSON.stringify(await read()) !== JSON.stringify(room)) throw new Error("Download owner changed while choosing its folder.");
          changes = directory === null ? null : { directory };
        } else changes = action === "reset-directory" ? { directory: null } : { promptForLocation: request.promptForLocation };
        snapshot = changes === null ? await host.downloadAction(room, "snapshot", undefined, undefined, validateCurrent) : await host.setDownloadPreferences(room, changes, validateCurrent);
      } else {
        if (request.promptForLocation !== undefined) throw new Error("Download action does not accept a prompt preference.");
        snapshot = await host.downloadAction(room, action, request.id === undefined ? undefined : identity(request.id), request.query, validateCurrent);
      }
      if (host !== getHost() || JSON.stringify(await read()) !== JSON.stringify(room)) throw new Error("Native download owner changed during the request; the outcome is uncertain.");
      return snapshot;
    },
    async closePage(token, sender, request) {
      requireAccess(token); record(request, ["home", "workerEnvironmentId", "surfaceId"]); requireHome(request.home);
      const workerEnvironmentId = identity(request.workerEnvironmentId), surfaceId = identity(request.surfaceId);
      const window = windowForSender(sender);
      if (!window || window.isDestroyed() || sender.isDestroyed() || window.webContents !== sender) throw new Error("Native browser page close requires its owning client window.");
      const surface = await resolveSurface(workerEnvironmentId, surfaceId);
      requireHome(request.home);
      if (surface.readOnly) throw new Error("This native browser surface is read-only.");
      const target = projection(workerEnvironmentId, surface);
      requireAccess(token); const host = getHost(); host.getTarget(target); await host.closeTarget(target);
    },
    async closeOwner(token) { await Promise.all([...attachments.values()].filter((entry) => entry.token === token).map(closeEntry)); },
    onEvent(event) {
      const entry = attachments.get(event?.attachmentId);
      if (!entry || entry.cancelled || event.hostGeneration !== entry.hostGeneration || event.mountGeneration !== entry.generation) return;
      try {
        requireAccess(entry.token); requireHome(entry.home);
        if (entry.sender.isDestroyed()) throw new Error("Native browser client window was destroyed.");
        if (event.type === "state") {
          checkState(entry, event.state);
          if (entry.state && (event.state.viewportSequence < entry.state.viewportSequence || event.state.modelViewport.revision < entry.state.modelViewport.revision)) return;
          entry.state = event.state;
        }
        entry.sender.send("octopus:native-browser-viewer-event", event);
        if (event.type === "closed" || event.type === "error") detach(entry);
      } catch (error) { void failed(entry, error); }
    },
    revalidate() {
      if (revalidation) { revalidateAgain = true; return revalidation; }
      revalidation = (async () => {
        do {
          revalidateAgain = false;
          let environments;
          try { environments = await resolveEnvironments(); }
          catch (error) { await Promise.all([...attachments.values()].map((entry) => failed(entry, error))); throw error; }
          const explicit = new Map();
          for (const environment of environments) {
            if (typeof environment.worker_environment_id === "string" && /^[a-f0-9]{64}$/.test(environment.formation_pin) && ["active", "released", "failed"].includes(environment.lifecycle)) explicit.set(environment.worker_environment_id, environment);
          }
          for (const [id, pin] of formations) {
            const row = explicit.get(id);
            if (row && (row.lifecycle !== "active" || row.formation_pin !== pin)) { formations.delete(id); await getHost().revokeFormation({ worker_environment_id: id, formation_pin: pin, reason: "Worker Environment formation withdrawn" }); }
          }
          for (const environment of environments) {
            if (explicit.has(environment.worker_environment_id) && environment.lifecycle === "active" && environment.client_surfaces?.some((surface) => surface.protocol === "browser.native.v1")) formations.set(environment.worker_environment_id, environment.formation_pin);
          }
          await Promise.all([...attachments.values()].filter((entry) => entry.prepared).map(async (entry) => { try { await current(entry); } catch (error) { reportError(error); } }));
        } while (revalidateAgain);
      })().catch(reportError).finally(() => { revalidation = null; });
      return revalidation;
    },
  };
  return api;
}


function createBrowserPresentationHost(options) {
  const { blake3 } = require('@noble/hashes/blake3');
  for (const name of ['homePath', 'requireAccess', 'readActivity', 'readArtifact', 'readToolDescriptor', 'currentRoom', 'currentTarget', 'getNativeHost', 'focusTarget']) {
    if (typeof options?.[name] !== 'function') throw new Error(`Browser preview requires ${name}`);
  }
  const images = new Map(), cursors = new Map();
  let home = null, native = null, closed = false;
  const snapshot = () => [...images.values()].map(record => ({ worker_environment_id: record.input.workerEnvironmentId, run_id: record.input.runId,
    target_id: record.projection.target_id, source_movement_seq: record.sequence, artifact_id: record.artifactId,
    phase: record.phase, error: record.error || null }));
  const changed = () => options.onChange?.();
  function access() {
    if (closed || home !== options.homePath()) throw new Error('Browser preview Home or owner changed');
    options.requireAccess();
  }
  function remove(record) {
    if (images.get(record.key) !== record) return;
    images.delete(record.key);
    if (record.published) native?.invalidateBrowserImage({ identity: record.identity });
    changed();
  }
  async function current(record) {
    access();
    const pin = await validateBrowserResultTool(options, () => options.currentRoom(record.owner), record.owner.room, record.toolId, 'screenshot', access);
    if (pin !== record.toolPin) throw new Error('Browser preview tool pin changed after capture');
    await options.currentTarget(record);
    access();
  }
  async function sourceImage(payload, row, room, turn) {
    if (typeof payload?.tool !== 'string' || payload.ok !== true || payload.error != null) return null;
    const body = payload.body;
    if (body?.result !== 'browser.screenshot' || body.worker_environment_id !== room.room.worker_environment_id) return null;
    const toolPin = await validateBrowserResultTool(options, () => options.currentRoom(room), room.room, payload.tool, 'screenshot', access);
    if (!Number.isSafeInteger(body.source_movement_seq) || body.source_movement_seq < Math.max(turn.turn_opened_movement_seq, room.room.opened_at_movement_seq)
      || body.source_movement_seq > row.movement_seq || !Number.isSafeInteger(body.bytes) || body.bytes <= 0 || body.bytes > 100000000
      || !/^[a-f0-9]{64}$/.test(body.content_hash || '') || !['image/png', 'image/jpeg'].includes(body.media_type)) throw new Error('Browser preview capture provenance is invalid');
    const surface = room.room.client_surfaces.find(value => value.protocol === 'browser.native.v1' && value.mode === 'direct' && value.data?.target_id === body.target_id);
    if (!surface) return null;
    if (!Array.isArray(body.media) || body.media.length !== 1 || body.media[0].mime_type !== body.media_type || typeof body.media[0].payload_base64 !== 'string') throw new Error('Browser preview capture has no exact image carrier');
    const encoded = body.media[0].payload_base64, image = Buffer.from(encoded, 'base64');
    if (image.toString('base64') !== encoded || image.length !== body.bytes || Buffer.from(blake3(image)).toString('hex') !== body.content_hash) throw new Error('Browser preview bytes do not match the admitted capture');
    return { image, surface, sequence: body.source_movement_seq, toolId: payload.tool, toolPin };
  }
  async function reconcile(rooms, turns) {
    if (closed) return;
    options.requireAccess();
    if (home !== null && home !== options.homePath()) { revokeAll(); throw new Error('Browser preview belongs to another Home; restart the client'); }
    home = options.homePath(); access();
    if (!Array.isArray(rooms) || rooms.length > 1024 || !Array.isArray(turns)) throw new Error('Browser preview source inventory is invalid');
    const byRun = new Map(turns.map(turn => [turn.run_id, turn]));
    const active = new Map();
    for (const owner of rooms) {
      const turn = byRun.get(owner.room.run_id);
      if (owner.room.lifecycle !== 'active' || !turn?.turn_open || !Number.isSafeInteger(turn.turn_opened_movement_seq)
        || !Number.isSafeInteger(turn.last_movement_seq) || turn.last_movement_seq < turn.turn_opened_movement_seq) continue;
      active.set(owner.room.worker_environment_id, { ...owner, turn });
    }
    for (const record of [...images.values()]) {
      const owner = active.get(record.input.workerEnvironmentId);
      if (!owner || record.batch !== owner.turn.turn_opened_movement_seq || owner.room.formation_pin !== record.projection.formation_pin
        || !owner.room.client_surfaces.some(surface => surface.protocol === 'browser.native.v1' && surface.data?.target_id === record.projection.target_id
          && surface.data?.host_generation === record.projection.host_generation && surface.data?.engine_handle === record.projection.engine_handle)) remove(record);
      else { try { await current(record); } catch { remove(record); } }
    }
    const activeRuns = new Set([...active.values()].map(owner => owner.room.run_id));
    for (const run of cursors.keys()) if (!activeRuns.has(run)) cursors.delete(run);
    for (const run of activeRuns) {
      const owners = [...active.values()].filter(owner => owner.room.run_id === run), turn = owners[0].turn;
      let cursor = cursors.get(run);
      if (!cursor || cursor.batch !== turn.turn_opened_movement_seq) { cursor = { batch: turn.turn_opened_movement_seq, after: turn.turn_opened_movement_seq - 1 }; cursors.set(run, cursor); }
      while (cursor.after < turn.last_movement_seq) {
        access();
        const activity = await options.readActivity(run, cursor.after, turn.last_movement_seq);
        access();
        if (!Array.isArray(activity.activity) || !(activity.latest_seq === null || Number.isSafeInteger(activity.latest_seq)) || typeof activity.has_newer !== 'boolean') throw new Error('Browser preview activity projection is incomplete');
        let last = cursor.after;
        for (const row of activity.activity) {
          if (!Number.isSafeInteger(row.movement_seq) || row.movement_seq <= last) throw new Error('Browser preview movement cursor is invalid');
          if (row.movement_seq > turn.last_movement_seq) break;
          last = Math.max(last, row.movement_seq);
          const source = owners.filter(owner => row.from?.kind === 'interface' && row.from.id === owner.room.door && row.run_id === run);
          if (!source.length || row.error && !row.payload_error || ['VerificationFailed', 'DeliveryRefused'].some(kind => String(row.movement_kind).startsWith(kind))) continue;
          if (!/^[a-f0-9]{64}$/.test(row.content_hash || '') || row.artifact_id !== `${row.label}:${row.content_hash}`) throw new Error('Browser preview has no exact admitted artifact');
          const artifact = await options.readArtifact(row); access();
          for (const owner of source) {
            const capture = await sourceImage(artifact.payload, row, owner, turn); if (!capture) continue;
            const data = capture.surface.data;
            const projection = { protocol: 'browser.native.v1', worker_environment_id: owner.room.worker_environment_id, formation_pin: owner.room.formation_pin,
              engine_handle: data.engine_handle, host_generation: data.host_generation, browser_context_id: data.browser_context_id, target_id: data.target_id };
            const key = JSON.stringify([run, owner.room.worker_environment_id, owner.room.formation_pin, data.host_generation, data.engine_handle, data.browser_context_id, data.target_id, cursor.batch]);
            let record = images.get(key);
            if (record && capture.sequence <= record.sequence) continue;
            if (!record) {
              record = { key, projection, input: { workerEnvironmentId: owner.room.worker_environment_id, surfaceId: capture.surface.id, runId: run }, batch: cursor.batch,
                identity: { hostGeneration: data.host_generation, presentationId: `browser:${key}`, streamGeneration: capture.sequence }, published: false };
              images.set(key, record);
            }
            record.sequence = capture.sequence; record.artifactId = row.artifact_id; record.owner = owner; record.toolId = capture.toolId; record.toolPin = capture.toolPin;
            try {
              await current(record);
              if (!native) native = await options.getNativeHost();
              access(); await current(record);
              if (typeof native?.upsertBrowserImage !== 'function' || typeof native.invalidateBrowserImage !== 'function') throw new Error('Client native browser preview API is unavailable');
              native.upsertBrowserImage({ identity: record.identity, groupId: run, batchId: String(cursor.batch), sequence: capture.sequence, image: capture.image });
              record.published = true; record.phase = 'published'; record.error = null; changed();
            } catch (error) { record.phase = 'failed'; record.error = error.message; changed(); }
          }
        }
        if (activity.latest_seq !== null) {
          if (activity.latest_seq < last) throw new Error('Browser preview source cursor contradicts its movements');
          last = Math.max(last, Math.min(turn.last_movement_seq, activity.latest_seq));
        }
        if (activity.has_newer && last <= cursor.after) throw new Error('Browser preview activity cursor did not advance');
        cursor.after = last;
        if (!activity.has_newer) { cursor.after = turn.last_movement_seq; break; }
      }
    }
  }
  async function onEvent(event) {
    if (event?.type !== 'focus-browser') return;
    const record = [...images.values()].find(value => value.published && JSON.stringify(value.identity) === JSON.stringify(event.identity));
    if (!record || event.groupId !== record.input.runId || event.batchId !== String(record.batch)) return;
    try { await current(record); await options.focusTarget(record); await current(record); }
    catch (error) { record.error = error.message; changed(); }
  }
  function revokeAll() { for (const record of [...images.values()]) remove(record); cursors.clear(); }
  function stop() { if (closed) return; revokeAll(); closed = true; }
  return { reconcile, onEvent, snapshot, revokeAll, stop };
}


function browserToolDescriptorPin(input) {
  const { blake3 } = require('@noble/hashes/blake3');
  const compare = (a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b));
  const canonical = value => Array.isArray(value) ? `[${value.map(canonical).join(',')}]` : value && typeof value === 'object'
    ? `{${Object.keys(value).sort(compare).map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}` : JSON.stringify(value);
  const object = value => { if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid pinned tool descriptor object'); return structuredClone(value); };
  const vector = value => { if (!Array.isArray(value)) throw new Error('Invalid pinned tool descriptor array'); return value; };
  const strings = value => { const values = vector(value); if (values.some(item => typeof item !== 'string')) throw new Error('Invalid pinned tool descriptor string set'); return [...new Set(values)].sort(compare); };
  const descriptor = object(input), process = object(descriptor.process), authority = object(process.authority);
  for (const field of ['read_root_slots', 'write_root_slots', 'executable_root_slots', 'read_paths', 'write_paths', 'executable_paths', 'executable_commands', 'read_env_paths', 'write_env_paths', 'executable_env_paths']) authority[field] = strings(authority[field]);
  for (const field of ['optional_read_root_slots', 'optional_write_root_slots', 'optional_executable_root_slots', 'optional_read_env_paths', 'optional_write_env_paths']) authority[field] = strings(authority[field] === undefined ? [] : authority[field]);
  const subpaths = vector(authority.executable_root_subpaths === undefined ? [] : authority.executable_root_subpaths).map(object);
  for (const item of subpaths) if (typeof item.root_slot !== 'string' || typeof item.relative_path !== 'string') throw new Error('Invalid executable root subpath');
  authority.executable_root_subpaths = [...new Map(subpaths.map(item => [canonical(item), item])).values()].sort((a, b) => compare(a.root_slot, b.root_slot) || compare(a.relative_path, b.relative_path));
  for (const field of ['read_env_subpaths', 'write_env_subpaths', 'executable_env_subpaths']) authority[field] = vector(authority[field] === undefined ? [] : authority[field]);
  for (const field of ['read_argument_paths', 'write_argument_paths', 'executable_argument_paths']) authority[field] = vector(authority[field] === undefined ? [] : authority[field]).map(value => {
    const item = object(value); item.within_root_slots = strings(item.within_root_slots); if (item.include_parent === undefined) item.include_parent = false; return item;
  });
  for (const field of ['allow_application_launch', 'allow_sandbox_delegation']) if (authority[field] === undefined) authority[field] = false;
  if (authority.sandbox_mode === undefined) authority.sandbox_mode = 'enforced';
  for (const field of ['required_files', 'inherit_env', 'inherit_env_from_interface', 'coordinate_root_slots']) process[field] = strings(process[field] === undefined ? [] : process[field]);
  const privateReads = vector(process.private_read_subpaths === undefined ? [] : process.private_read_subpaths).map(object);
  for (const item of privateReads) {
    if (typeof item.path !== 'string') throw new Error('Invalid private read subpath');
    if (item.optional !== undefined && typeof item.optional !== 'boolean') throw new Error('Invalid private read subpath optional flag');
    if (item.optional === false) delete item.optional;
  }
  process.private_read_subpaths = [...new Map(privateReads.map(item => [canonical(item), item])).values()].sort((a, b) => compare(a.path, b.path) || Number(Boolean(a.optional)) - Number(Boolean(b.optional)));
  process.args = vector(process.args === undefined ? [] : process.args); process.env = object(process.env === undefined ? {} : process.env);
  if (process.interrupted_effect_recovery === undefined) process.interrupted_effect_recovery = 'indeterminate';
  process.authority = authority; descriptor.process = process;
  if (descriptor.handler_config === undefined) descriptor.handler_config = null;
  if (descriptor.description === undefined) descriptor.description = '';
  if (descriptor.args_schema === undefined) descriptor.args_schema = { type: 'object', properties: {} };
  if (descriptor.is_world_effect === undefined) descriptor.is_world_effect = false;
  return Buffer.from(blake3(Buffer.from(canonical(descriptor)))).toString('hex');
}

async function validateBrowserResultTool(options, currentRoom, room, toolId, operation, current) {
  current();
  const formed = await currentRoom(); current();
  const pin = formed?.tool_descriptor_hashes?.[toolId];
  if (!Array.isArray(formed?.tools) || !formed.tools.includes(toolId) || !/^[a-f0-9]{64}$/.test(pin || '')) throw new Error('Browser result tool is not pinned in its current formed grant');
  const descriptor = await options.readToolDescriptor(toolId); current();
  if (descriptor?.id !== toolId || descriptor.interface !== room.door
    || descriptor.handler_config?.operation !== operation
    || JSON.stringify(descriptor.supported_worker_environment_kinds) !== JSON.stringify(['browser_session'])
    || browserToolDescriptorPin(descriptor) !== pin) throw new Error('Browser result does not match its exact declared tool operation, door and formation pin');
  return pin;
}

function createBrowserDownloadHistory(options) {
  for (const name of ['homePath', 'requireAccess', 'readActivity', 'readArtifact', 'readToolDescriptor', 'currentRoom', 'acceptArtifact']) {
    if (typeof options?.[name] !== 'function') throw new Error(`Browser download history requires ${name}`);
  }
  let home = null, stopped = false;
  const cursors = new Map();
  function current() {
    if (stopped || options.homePath() !== home) throw new Error('Browser download history Home changed');
    options.requireAccess();
  }
  async function reconcile(rooms, turns) {
    if (stopped) return;
    options.requireAccess();
    if (home !== null && home !== options.homePath()) { cursors.clear(); throw new Error('Browser download history requires its original Home'); }
    home = options.homePath(); current();
    const byRun = new Map(turns.map(turn => [turn.run_id, turn])), active = new Set();
    for (const owner of rooms) {
      const room = owner.room, through = byRun.get(room.run_id)?.last_movement_seq;
      if (!Number.isSafeInteger(through) || !Number.isSafeInteger(room.opened_at_movement_seq)) continue;
      const data = owner.surface.data, projection = { protocol: 'browser.native.downloads.v1', worker_environment_id: room.worker_environment_id,
        formation_pin: room.formation_pin, host_generation: data.host_generation };
      const key = JSON.stringify([room.run_id, projection]); active.add(key);
      let cursor = cursors.get(key);
      if (!cursor) { cursor = { after: Math.max(0, room.opened_at_movement_seq - 1) }; cursors.set(key, cursor); }
      while (cursor.after < through) {
        current(); await options.currentRoom(owner, projection); current();
        const activity = await options.readActivity(room.run_id, cursor.after); current();
        if (!Array.isArray(activity.activity) || !(activity.latest_seq === null || Number.isSafeInteger(activity.latest_seq)) || typeof activity.has_newer !== 'boolean') throw new Error('Browser download activity projection is incomplete');
        let last = cursor.after;
        for (const row of activity.activity) {
          if (!Number.isSafeInteger(row.movement_seq) || row.movement_seq <= last) throw new Error('Browser download activity cursor is invalid');
          if (row.movement_seq > through) break;
          last = row.movement_seq;
          if (row.from?.kind !== 'interface' || row.from.id !== room.door || row.run_id !== room.run_id
            || row.error && !row.payload_error || ['VerificationFailed', 'DeliveryRefused'].some(kind => String(row.movement_kind).startsWith(kind))) continue;
          if (!/^[a-f0-9]{64}$/.test(row.content_hash || '') || row.artifact_id !== `${row.label}:${row.content_hash}`) throw new Error('Browser download result has no admitted artifact identity');
          const artifact = await options.readArtifact(row); current();
          const payload = artifact.payload, body = payload?.body;
          if (typeof payload?.tool !== 'string' || payload.ok !== true || payload.error != null || body?.result !== payload.tool
            || body.status !== 'completed' || body.worker_environment_id !== room.worker_environment_id) continue;
          const validateTool = () => validateBrowserResultTool(options, () => options.currentRoom(owner, projection), room, payload.tool, 'download_wait', current);
          const toolPin = await validateTool();
          if (!Number.isSafeInteger(body.source_movement_seq) || body.source_movement_seq < room.opened_at_movement_seq || body.source_movement_seq > row.movement_seq
            || body.arm?.formation_pin !== room.formation_pin) throw new Error('Browser download artifact belongs to another formation');
          await options.currentRoom(owner, projection); current();
          await options.acceptArtifact(projection, body, async () => { current(); if (await validateTool() !== toolPin) throw new Error('Browser download tool pin changed during collection'); current(); }); current();
          await options.currentRoom(owner, projection); current();
        }
        if (activity.latest_seq !== null) {
          if (activity.latest_seq < last) throw new Error('Browser download source cursor contradicts its movements');
          last = Math.max(last, Math.min(through, activity.latest_seq));
        }
        if (activity.has_newer && last <= cursor.after) throw new Error('Browser download activity cursor did not advance');
        cursor.after = activity.has_newer ? last : through;
        if (!activity.has_newer) break;
      }
    }
    for (const key of cursors.keys()) if (!active.has(key)) cursors.delete(key);
  }
  function revokeAll() { cursors.clear(); }
  function stop() { stopped = true; revokeAll(); }
  return { reconcile, revokeAll, stop };
}

module.exports = { createNativeBrowserSurfaceAdapter, createBrowserPresentationHost, createBrowserDownloadHistory, browserToolDescriptorPin };
