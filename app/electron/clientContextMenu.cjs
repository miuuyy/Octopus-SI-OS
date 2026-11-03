const REQUEST_ID = /^[A-Za-z0-9:_-]{1,128}$/;
const ACCELERATOR = /^(?:(?:CommandOrControl|CmdOrCtrl|Command|Cmd|Control|Ctrl|Alt|Option|Shift|Super)\+){0,4}(?:[A-Z0-9]|F(?:[1-9]|1[0-9]|2[0-4])|Space|Tab|Enter|Escape|Backspace|Delete|Insert|Home|End|PageUp|PageDown|Up|Down|Left|Right)$/;

function fields(value, allowed) {
  if (!value || typeof value !== "object" || Array.isArray(value)
    || ![Object.prototype, null].includes(Object.getPrototypeOf(value))
    || Object.keys(value).some((key) => !allowed.includes(key))) {
    throw new Error("Invalid native context-menu fields.");
  }
}

function requestId(value) {
  if (typeof value !== "string" || !REQUEST_ID.test(value)) throw new Error("Invalid native context-menu request ID.");
  return value;
}

function validateRequest(value) {
  fields(value, ["requestId", "items", "position"]);
  const id = requestId(value.requestId), ids = new Set();
  let count = 0, textLength = 0;
  const items = (rows, depth = 0) => {
    if (!Array.isArray(rows) || !rows.length || rows.length > 256 || depth > 8) throw new Error("Native context menu exceeds item or nesting bounds.");
    return rows.map((item) => {
      if (++count > 256) throw new Error("Native context menu exceeds 256 items.");
      fields(item, item?.type === "separator" ? ["type"] : ["id", "label", "type", "enabled", "checked", "submenu", "accelerator"]);
      if (item.type === "separator") return { type: "separator" };
      const id = item.id;
      if (typeof id !== "string" || !id.length || id.length > 256 || /[\x00-\x1f\x7f]/.test(id)) throw new Error("Invalid native menu item ID.");
      if (ids.has(id)) throw new Error("Duplicate native context-menu item ID.");
      ids.add(id);
      if (typeof item.label !== "string" || !item.label.trim() || item.label.length > 180 || /[\x00-\x1f\x7f]/.test(item.label)) throw new Error("Invalid native context-menu label.");
      textLength += item.label.length;
      if (textLength > 32768) throw new Error("Native context-menu text exceeds bounds.");
      const type = item.type ?? "normal";
      if (!["normal", "checkbox", "radio"].includes(type)) throw new Error("Invalid native context-menu item type.");
      if (item.enabled !== undefined && typeof item.enabled !== "boolean") throw new Error("Invalid native menu enabled state.");
      if (item.checked !== undefined && (typeof item.checked !== "boolean" || type === "normal")) throw new Error("Invalid native menu checked state.");
      if (item.accelerator !== undefined && (typeof item.accelerator !== "string" || item.accelerator.length > 80 || !ACCELERATOR.test(item.accelerator))) throw new Error("Invalid native menu accelerator.");
      if (item.submenu !== undefined && (type !== "normal" || item.accelerator !== undefined)) throw new Error("Invalid native submenu item.");
      return { id, label: item.label, type, enabled: item.enabled !== false,
        ...(type !== "normal" ? { checked: item.checked === true } : {}),
        ...(item.accelerator ? { accelerator: item.accelerator } : {}),
        ...(item.submenu !== undefined ? { submenu: items(item.submenu, depth + 1) } : {}),
      };
    });
  };
  let position;
  if (value.position !== undefined) {
    fields(value.position, ["x", "y"]);
    if (![value.position.x, value.position.y].every((n) => typeof n === "number" && Number.isFinite(n) && n >= 0 && n <= 32768)) throw new Error("Invalid native menu position.");
    position = { x: value.position.x, y: value.position.y };
  }
  return { requestId: id, items: items(value.items), position };
}

function createClientContextMenuHost({ Menu, resolveOwner, reportError }) {
  const active = new Map();
  function owner(event) {
    const sender = event.sender, frame = event.senderFrame, win = resolveOwner(event);
    if (!win || win.isDestroyed() || !sender || sender.isDestroyed() || win.webContents !== sender
      || !frame || frame !== sender.mainFrame || frame.detached) throw new Error("Native context menu requires the trusted current main client frame.");
    return win;
  }
  function cancel(event, value) {
    fields(value, ["requestId"]);
    const id = requestId(value.requestId);
    owner(event);
    const pending = active.get(event.sender);
    if (pending?.requestId !== id) return { cancelled: false };
    pending.cancel();
    return { cancelled: true };
  }
  function show(event, value) {
    const win = owner(event), request = validateRequest(value), sender = event.sender, frame = event.senderFrame;
    const previous = active.get(sender);
    if (previous?.requestId === request.requestId) throw new Error("Native menu request is already open.");
    previous?.cancel();
    return new Promise((resolve, reject) => {
      let done = false, menu;
      const subscriptions = [];
      const finish = (id, error) => {
        if (done) return;
        done = true;
        if (active.get(sender) === pending) active.delete(sender);
        for (const [target, name, callback] of subscriptions) target.removeListener(name, callback);
        if (error) reject(error);
        else resolve({ requestId: request.requestId, id });
      };
      const close = () => {
        finish(null);
        if (menu && !win.isDestroyed()) menu.closePopup(win);
      };
      const pending = { requestId: request.requestId, cancel: close };
      const isCurrent = () => {
        try { return !done && active.get(sender) === pending && frame === sender.mainFrame && owner(event) === win; }
        catch { return false; }
      };
      const convert = (items, enabled = true) => items.map((item) => {
        if (item.type === "separator") return item;
        const selectable = enabled && item.enabled;
        // Electron 45.2.1's Cocoa NSMenu path also calls FixUpWindowsStyleLabel:
        // Chromium 156.0.8078.75 decodes && to & before constructing NSMenuItem.
        return { ...item, label: item.label.replaceAll("&", "&&"), registerAccelerator: false,
          ...(item.submenu ? { type: "submenu", submenu: convert(item.submenu, selectable) } : {
            click: () => { if (selectable) finish(isCurrent() ? item.id : null); },
          }),
        };
      });
      const listen = (target, name, callback) => {
        target.on(name, callback);
        subscriptions.push([target, name, callback]);
      };
      const cancelOnLifetimeEnd = () => { try { close(); } catch (error) { reportError(error); } };
      active.set(sender, pending);
      listen(sender, "destroyed", cancelOnLifetimeEnd);
      listen(sender, "render-process-gone", cancelOnLifetimeEnd);
      listen(sender, "did-start-navigation", (_event, _url, _inPlace, isMainFrame) => { if (isMainFrame) cancelOnLifetimeEnd(); });
      listen(win, "closed", cancelOnLifetimeEnd);
      try {
        menu = Menu.buildFromTemplate(convert(request.items));
        let position = {};
        if (request.position) {
          const zoom = sender.getZoomFactor(), bounds = win.getContentBounds();
          if (!Number.isFinite(zoom) || zoom <= 0) throw new Error("Invalid native menu window zoom.");
          position = {
            x: Math.round(Math.max(0, Math.min(bounds.width - 1, request.position.x * zoom))),
            y: Math.round(Math.max(0, Math.min(bounds.height - 1, request.position.y * zoom))),
          };
        }
        menu.popup({ window: win, ...position, callback: () => finish(null) });
      } catch (error) { finish(null, error); }
    });
  }
  return { show, cancel };
}

module.exports = { createClientContextMenuHost };
