const fs = require("node:fs/promises");
const { constants } = require("node:fs");
const path = require("node:path");
const { collectSupportReport, supportReportText, supportIssueUrl } = require("./clientSupportReport.cjs");

function createClientSupportHost({ app, BrowserWindow, ipcMain, dialog, clipboard, shell, clientRoot, application, accessStatus, runtime, progress, devUrl }) {
  const windows = new Map();
  function entry(event) {
    const value = windows.get(event.sender.id);
    if (!value || event.senderFrame !== event.sender.mainFrame) throw new Error("This action belongs to the problem report window.");
    return value;
  }
  function profile(value) {
    const status = accessStatus(value.token);
    return status.authenticated ? status.profile : null;
  }
  function snapshot(value, request) {
    if (!value.report || request?.reportId !== value.report.report_id) throw new Error("Refresh the report before continuing.");
    if (value.report.access !== (profile(value) ?? "locked")) throw new Error("Client access changed. Close this window and reopen Help → Report a Problem.");
    if (typeof request.description !== "string" || request.description.length > 4000) throw new Error("Describe the problem in 4,000 characters or fewer.");
    return supportReportText(value.report, request.description);
  }
  ipcMain.handle("octopus:support-collect", async (event, request) => {
    const value = entry(event);
    if (typeof request?.includeDetails !== "boolean") throw new Error("Choose whether to include error text.");
    if (request.includeDetails && profile(value) !== "live") throw new Error("Unlock Octopus as Administrator to include error text.");
    const generation = ++value.generation;
    const report = await collectSupportReport({ application: application(), clientRoot, runtime: runtime(), progress: progress(), profile: profile(value), includeDetails: request.includeDetails });
    if (generation !== value.generation) throw new Error("A newer report is being prepared.");
    if (report.access !== (profile(value) ?? "locked")) throw new Error("Client access changed while collecting the report.");
    value.report = report;
    return report;
  });
  ipcMain.handle("octopus:support-copy", async (event, request) => {
    await clipboard.writeText(snapshot(entry(event), request));
    return { copied: true };
  });
  ipcMain.handle("octopus:support-save", async (event, request) => {
    const value = entry(event);
    const text = snapshot(value, request);
    const selected = await dialog.showSaveDialog(value.win, {
      title: "Save problem report",
      defaultPath: `Octopus-report-${value.report.report_id}.txt`,
      filters: [{ name: "Text report", extensions: ["txt"] }],
    });
    if (selected.canceled || !selected.filePath) return { saved: false };
    snapshot(value, request);
    const file = await fs.open(selected.filePath, constants.O_WRONLY | constants.O_CREAT | constants.O_TRUNC | constants.O_NOFOLLOW, 0o600);
    try {
      await file.chmod(0o600);
      await file.writeFile(text, "utf8");
    } finally { await file.close(); }
    return { saved: true };
  });
  ipcMain.handle("octopus:support-issue", async (event, request) => {
    const value = entry(event);
    snapshot(value, request);
    await shell.openExternal(supportIssueUrl(value.report, request.description));
    return { opened: true };
  });

  function open(token = "") {
    const focused = BrowserWindow.getFocusedWindow();
    if (focused && windows.has(focused.webContents.id)) { focused.show(); focused.focus(); return; }
    const existing = [...windows.values()].find((value) => value.token === token && !value.win.isDestroyed());
    if (existing) { existing.win.show(); existing.win.focus(); return; }
    const win = new BrowserWindow({
      title: "Report a Problem", width: 660, height: 750, minWidth: 520, minHeight: 560,
      backgroundColor: "#181818", show: false, autoHideMenuBar: true,
      webPreferences: { preload: path.join(__dirname, "supportPreload.cjs"), contextIsolation: true, nodeIntegration: false, sandbox: true },
    });
    windows.set(win.webContents.id, { win, token, report: null, generation: 0 });
    win.once("closed", () => windows.delete(win.webContents.id));
    win.webContents.setWindowOpenHandler(() => ({ action: "deny" }));
    win.webContents.on("will-navigate", (event) => event.preventDefault());
    win.once("ready-to-show", () => { win.show(); win.focus(); });
    const loaded = devUrl ? win.loadURL(new URL("support.html", devUrl).href) : win.loadFile(path.join(__dirname, "..", "dist", "support.html"));
    void loaded.catch(async () => {
      await dialog.showMessageBox(win, { type: "error", title: "Problem report unavailable", message: "The report window could not load.", detail: "Client logs remain in the selected Home's client/logs directory. See Troubleshooting in the Help menu." });
      win.destroy();
    });
  }

  function closeOwner(token) {
    for (const value of windows.values()) if (value.token === token && !value.win.isDestroyed()) value.win.destroy();
  }

  return { open, closeOwner };
}

module.exports = { createClientSupportHost };
