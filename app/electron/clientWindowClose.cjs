const { randomUUID } = require("node:crypto");

function createClientWindowCloseHost({ authorize, accessTokenForSender, onQuitApproved, onQuitCancelled, reportError = console.error }) {
  const windows = new Map();
  let action = null;
  let committed = false;

  function send(record, channel, payload) {
    if (record.win.isDestroyed() || record.sender.isDestroyed()) return;
    try { record.sender.send(channel, payload); }
    catch (error) { reportError(error); }
  }

  function cancelRequest(record, reason) {
    const request = record.request;
    record.request = null;
    if (request) send(record, "octopus:client-window-close-cancelled", { ...request.payload, reason });
  }

  function cancelAction(reason) {
    const current = action;
    action = null;
    if (current) for (const record of current.windows) cancelRequest(record, reason);
    current?.complete?.({ accepted: false });
    if (current?.reason === "quit") onQuitCancelled?.(reason);
  }

  function invalidate(record) {
    if (action?.windows.has(record)) cancelAction("invalidated");
    cancelRequest(record, "invalidated");
    record.guard = null;
    record.generation += 1;
    record.readiness = record.protectedDocument ? "unregistered" : "initializing";
  }

  function finishQuit(current) {
    if (action !== current || current.phase !== "closing" || current.windows.size) return;
    action = null;
    committed = true;
    onQuitApproved();
  }

  function remove(record) {
    if (windows.get(record.sender) !== record) return;
    windows.delete(record.sender);
    if (action?.windows.has(record)) {
      if (action.phase === "collecting") cancelAction("invalidated");
      else {
        const current = action;
        current.windows.delete(record);
        finishQuit(current);
      }
    }
    record.request = null;
    record.guard = null;
    for (const [emitter, event, listener] of record.listeners) emitter.removeListener(event, listener);
  }

  function startRequest(record, reason, transaction = null) {
    if (record.readiness !== "guarded" || !record.guard) return false;
    const payload = { windowId: record.windowId, generation: record.guard.generation, requestId: randomUUID(), reason };
    record.request = { payload, status: "pending", transaction };
    try { record.sender.send("octopus:client-window-close-requested", payload); }
    catch (error) {
      if (transaction) cancelAction("invalidated");
      else cancelRequest(record, "invalidated");
      reportError(error);
      return false;
    }
    return true;
  }

  function trackWindow(win, windowId) {
    if (committed) throw new Error("The client is already closing.");
    if (windows.has(win.webContents)) throw new Error("Window close lifetime is already tracked.");
    if (action && (action.reason === "quit" || accessTokenForSender(win.webContents) === action.token)) cancelAction("invalidated");
    const record = { win, sender: win.webContents, windowId, generation: 0, readiness: "initializing", protectedDocument: false, navigating: false, guard: null, request: null, listeners: [] };
    windows.set(record.sender, record);
    function on(emitter, event, listener) { emitter.on(event, listener); record.listeners.push([emitter, event, listener]); }
    on(win, "close", (event) => {
      const request = record.request;
      if (request?.status === "approved" && (request.payload.reason === "close" || action === request.transaction && action?.phase === "closing")) {
        request.status = "closing";
        queueMicrotask(() => {
          if (event.defaultPrevented && record.request === request) {
            if (request.transaction) cancelAction("cancelled");
            else cancelRequest(record, "cancelled");
          }
        });
        return;
      }
      if (["initializing", "settled", "renderer-gone"].includes(record.readiness) && (!action?.windows.has(record) || action.phase !== "collecting")) return;
      event.preventDefault();
      if (!action?.windows.has(record) && !record.request) startRequest(record, "close");
    });
    on(win, "closed", () => remove(record));
    on(record.sender, "destroyed", () => remove(record));
    on(record.sender, "render-process-gone", () => {
      invalidate(record);
      record.readiness = "renderer-gone";
      record.protectedDocument = false;
    });
    on(record.sender, "did-start-navigation", (event, _url, isInPlace, isMainFrame) => {
      const sameDocument = typeof event?.isSameDocument === "boolean" ? event.isSameDocument : Boolean(isInPlace);
      const mainFrame = typeof event?.isMainFrame === "boolean" ? event.isMainFrame : Boolean(isMainFrame);
      if (!mainFrame || sameDocument) return;
      invalidate(record);
      record.navigating = true;
    });
    on(record.sender, "did-navigate", () => {
      record.navigating = false;
      record.protectedDocument = false;
      record.readiness = "initializing";
    });
    on(record.sender, "did-fail-load", (_event, _errorCode, _errorDescription, _validatedURL, isMainFrame) => {
      if (!isMainFrame) return;
      if (typeof record.sender.isLoadingMainFrame === "function" && record.sender.isLoadingMainFrame()) return;
      record.navigating = false;
    });
    on(record.sender, "did-stop-loading", () => {
      if (!record.navigating) return;
      if (typeof record.sender.isLoadingMainFrame === "function" && record.sender.isLoadingMainFrame()) return;
      record.navigating = false;
    });
    on(record.sender, "will-prevent-unload", () => {
      if (action?.windows.has(record)) cancelAction("cancelled");
      else cancelRequest(record, "cancelled");
    });
  }

  function owned(sender) {
    const record = windows.get(sender);
    if (!record || record.win.isDestroyed() || sender.isDestroyed()) throw new Error("Window close guard requires its current client window.");
    return record;
  }

  function register(sender, token) {
    const record = owned(sender);
    authorize(sender, token);
    if (committed || record.navigating) throw new Error("Window close guard cannot register during window teardown or navigation.");
    invalidate(record);
    record.guard = { token, generation: record.generation };
    record.protectedDocument = true;
    record.readiness = "guarded";
    return { windowId: record.windowId, generation: record.guard.generation };
  }

  function unregister(sender, token, value) {
    const record = windows.get(sender);
    if (record?.guard?.token === token && value?.windowId === record.windowId && value?.generation === record.guard.generation) invalidate(record);
  }

  function advanceAction(current) {
    if (action !== current || !current.prepared || current.phase !== "collecting") return;
    for (const record of current.windows) {
      if (["initializing", "settled", "renderer-gone"].includes(record.readiness)) continue;
      if (record.request?.transaction !== current || record.request.status !== "approved") return;
    }
    if (current.reason === "logout") {
      const owners = [...windows.values()].filter((record) => accessTokenForSender(record.sender) === current.token);
      if (owners.length !== current.windows.size || owners.some((record) => !current.windows.has(record))) { cancelAction("invalidated"); return; }
      let value;
      try { value = current.commit(); }
      catch (error) {
        action = null;
        for (const record of current.windows) cancelRequest(record, "invalidated");
        current.reject(error);
        return;
      }
      action = null;
      for (const record of current.windows) {
        record.request = null;
        record.guard = null;
        record.generation += 1;
        record.protectedDocument = false;
        record.readiness = "settled";
      }
      current.complete({ accepted: true, value });
      return;
    }
    current.phase = "closing";
    setImmediate(() => {
      if (action !== current) return;
      for (const record of [...current.windows]) {
        if (action !== current) return;
        record.win.close();
      }
      finishQuit(current);
    });
  }

  function resolve(sender, token, value) {
    const record = windows.get(sender);
    const request = record?.request;
    if (!request || request.status !== "pending" || record.guard?.token !== token
      || value?.windowId !== record.windowId || value?.generation !== record.guard.generation
      || value?.requestId !== request.payload.requestId || typeof value.allowClose !== "boolean") return { accepted: false };
    try { owned(sender); }
    catch (error) { invalidate(record); reportError(error); return { accepted: false }; }
    if (!value.allowClose) {
      if (request.transaction) cancelAction("cancelled");
      else cancelRequest(record, "cancelled");
      return { accepted: true };
    }
    request.status = "approved";
    if (request.transaction) advanceAction(request.transaction);
    else setImmediate(() => {
      if (record.request === request && request.status === "approved") record.win.close();
    });
    return { accepted: true };
  }

  function requestQuit() {
    if (committed || action?.reason === "quit") return;
    if (action) cancelAction("invalidated");
    for (const record of windows.values()) cancelRequest(record, "invalidated");
    const current = { windows: new Set(windows.values()), reason: "quit", phase: "collecting", prepared: false };
    action = current;
    for (const record of current.windows) {
      if (["initializing", "settled", "renderer-gone"].includes(record.readiness)) continue;
      if (!startRequest(record, "quit", current)) { cancelAction("invalidated"); return; }
      if (action !== current) return;
    }
    current.prepared = true;
    advanceAction(current);
  }

  function requestLogout(sender, token, commit) {
    owned(sender);
    if (!token || accessTokenForSender(sender) !== token) throw new Error("Logout requires the current window's access owner.");
    if (committed || action?.reason === "quit") throw new Error("The client is closing its windows.");
    if (action) {
      if (action.token === token) return action.promise;
      throw new Error("Another window logout is pending.");
    }
    const owners = new Set([...windows.values()].filter((record) => accessTokenForSender(record.sender) === token));
    const current = { windows: owners, reason: "logout", token, commit, phase: "collecting", prepared: false };
    current.promise = new Promise((complete, reject) => { current.complete = complete; current.reject = reject; });
    action = current;
    for (const record of owners) {
      cancelRequest(record, "invalidated");
      if (["initializing", "settled", "renderer-gone"].includes(record.readiness)) continue;
      if (record.guard?.token !== token || !startRequest(record, "logout", current)) { cancelAction("invalidated"); break; }
      if (action !== current) break;
    }
    current.prepared = true;
    advanceAction(current);
    return current.promise;
  }

  function assertCanCreateWindow() {
    if (committed || action?.phase === "closing") throw new Error("The client is closing its windows.");
  }

  return { assertCanCreateWindow, register, requestLogout, requestQuit, resolve, trackWindow, unregister };
}

module.exports = { createClientWindowCloseHost };
