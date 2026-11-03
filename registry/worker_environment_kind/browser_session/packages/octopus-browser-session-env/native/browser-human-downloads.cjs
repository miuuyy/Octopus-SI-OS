"use strict";

const fs = require("node:fs/promises");
const nativeFs = require("node:fs");
const path = require("node:path");
const crypto = require("node:crypto");
const FINISHED = new Set(["complete", "failed", "canceled", "unavailable"]);
const HISTORY_LIMIT = 200;
const HISTORY_BYTES = 16 * 1024 * 1024;

function createHumanDownloads({ hostRoot, electron, emit, historyBackend = null }) {
  const records = new Map(), live = new Map(), modelItems = new WeakSet(), stores = new Map(), configured = new WeakSet();
  let sequence = 0;
  let preferences = { directory: null, promptForLocation: false }, preferencesError = null, preferenceWrites = Promise.resolve();
  const preferencesFile = path.join(hostRoot, "download-preferences.json"), activeSavePaths = new Set();
  const roomOwner = room => ({ workerEnvironmentId: room.formation.worker_environment_id, formationPin: room.formation.formation_pin });
  const scope = owner => JSON.stringify([owner.workerEnvironmentId, owner.formationPin]);
  const belongs = (owner, record) => record.scope === scope(owner);
  const check = (condition, message) => { if (!condition) throw Object.assign(new Error(message), { code: "download_action_failed" }); };
  if (historyBackend) check(historyBackend.api === "octopus.standardDownloadHistory.v1"
    && ["open", "configure", "identify", "record", "decode", "validate"].every(name => typeof historyBackend[name] === "function")
    && historyBackend.identityField === "standardId" && historyBackend.directoryName === "standard-download-history", "Unknown download history backend");
  const historyId = record => historyBackend ? record.standardId : record.nativeGuid;
  function orderedRecords(owner) {
    return [...records.values()].filter(record => belongs(owner, record)).sort((a, b) => b.startedAtMs - a.startedAtMs || a.id.localeCompare(b.id));
  }
  function retainedRecords(owner) {
    const sorted = orderedRecords(owner), active = sorted.filter(record => live.has(record.id));
    return [...active, ...sorted.filter(record => !live.has(record.id)).slice(0, Math.max(0, HISTORY_LIMIT - active.length))];
  }
  function privateDirectory(directory) {
    try { nativeFs.mkdirSync(directory, { mode: 0o700 }); } catch (error) { if (error.code !== "EEXIST") throw error; }
    const stat = nativeFs.lstatSync(directory);
    check(stat.isDirectory() && !stat.isSymbolicLink() && stat.uid === process.getuid() && (stat.mode & 0o777) === 0o700
      && nativeFs.realpathSync(directory) === directory, "Native download history directory must be a canonical private owner directory");
  }
  function nativeRecord(owner, row) {
    if (historyBackend) return historyBackend.decode(owner, row);
    check(row && typeof row.metadata === "string" && Buffer.byteLength(row.metadata) <= HISTORY_BYTES, "Invalid native history metadata");
    const value = JSON.parse(row.metadata), record = value?.record, native = row.native;
    check(value.schema_version === 1 && Object.keys(value).length === 2 && native && typeof native.guid === "string"
      && /^[a-f0-9]{8}(-[a-f0-9]{4}){3}-[a-f0-9]{12}$/i.test(native.guid) && record?.nativeGuid === native.guid
      && belongs(owner, record) && typeof record.targetId === "string" && typeof record.filename === "string" && typeof record.url === "string"
      && typeof record.path === "string" && (!record.path || path.isAbsolute(record.path))
      && typeof record.userInitiated === "boolean" && typeof record.unacknowledged === "boolean"
      && (record.userInitiated ? record.id === native.guid : /^[a-f0-9]{32}$/.test(record.id) && record.modelReceipt?.download_id === record.id && record.modelReceipt?.target_id === record.targetId)
      && [record.startedAtMs, record.updatedAtMs, record.receivedBytes, record.totalBytes, record.sequence].every(number => Number.isFinite(number) && number >= 0)
      && ["in_progress", "complete", "canceled", "interrupted"].includes(native.state), "Native download history changed its original owner or GUID");
    const received = Number(native.received_bytes), total = Number(native.total_bytes);
    check(/^[0-9]+$/.test(native.received_bytes) && /^-?[0-9]+$/.test(native.total_bytes)
      && Number.isSafeInteger(received) && received >= 0 && Number.isSafeInteger(total) && total >= -1, "Native download byte counters are invalid");
    record.receivedBytes = received; record.totalBytes = Math.max(0, total);
    record.sequence = 0; record.unacknowledged = false;
    if (native.state === "canceled") record.status = "canceled";
    else if (native.state === "interrupted") record.status = "failed";
    else if (native.state === "in_progress") record.status = "unavailable";
    else if (record.userInitiated || ["completed", "imported"].includes(record.modelReceipt?.status)) record.status = "complete";
    else record.status = "unavailable";
    if (record.userInitiated) {
      check(typeof native.target_path === "string" && (!native.target_path || path.isAbsolute(native.target_path)), "Native download path is invalid");
      record.path = native.target_path;
    }
    return record;
  }
  function storeFor(owner) {
    const key = scope(owner); let store = stores.get(key); if (store) return store;
    const root = path.join(hostRoot, historyBackend ? historyBackend.directoryName : "download-history"); privateDirectory(root);
    const directory = path.join(root, crypto.createHash("sha256").update(key).digest("hex")); privateDirectory(directory);
    store = { directory, reader: null, ready: null, error: null, writes: new Set(), removing: new Set(), actions: Promise.resolve() };
    stores.set(key, store);
    store.ready = (historyBackend ? historyBackend.open({ directory, owner }) : electron.session.openOwnedDownloadHistory(directory)).then(async reader => {
      for (const name of ["search", "update", "remove", "clear", "flush", "close"]) check(typeof reader?.[name] === "function", "Native history reader API differs");
      store.reader = reader;
      const result = await reader.search({ text: "", offset: 0, maxResults: 0 });
      check(Array.isArray(result.records) && result.hasMore === false && result.offset === 0 && result.text === "", "Native history initialization is incomplete");
      for (const row of result.records) {
        const record = nativeRecord(owner, row), existing = records.get(record.id);
        if (existing) check(belongs(owner, existing) && historyId(existing) === historyId(record), "Download history duplicated another owner");
        else records.set(record.id, record);
      }
      return store;
    }).catch(error => { historyFailure(owner, store, error); throw error; });
    store.ready.catch(() => {}); return store;
  }
  function historyFailure(owner, store, error) {
    const message = `${historyBackend ? "Standard" : "Native"} download history is unavailable: ${error.message}`;
    if (store.error === message) return;
    store.error = message; emit({ type: "download-history-error", workerEnvironmentId: owner.workerEnvironmentId, formationPin: owner.formationPin,
      error: { code: "download_history_unavailable", message } });
  }
  function track(owner, store, promise) {
    const pending = promise.catch(error => historyFailure(owner, store, error)).finally(() => store.writes.delete(pending));
    store.writes.add(pending); return pending;
  }
  function configureHistory(room) {
    const owner = roomOwner(room), store = storeFor(owner);
    if (!configured.has(room.session)) {
      if (historyBackend) track(owner, store, historyBackend.configure(room, store.directory));
      else {
        check(typeof room.session.configureOwnedDownloadHistory === "function" && typeof room.session.recordOwnedDownloadHistory === "function", "Admitted native Session has no history API");
        track(owner, store, room.session.configureOwnedDownloadHistory(store.directory));
      }
      configured.add(room.session);
    }
    return store;
  }
  function persistRecord(owner, record) {
    const store = storeFor(owner), entry = live.get(record.id);
    if (store.removing.has(historyId(record))) return;
    if (entry) track(owner, store, historyBackend
      ? historyBackend.record(entry.room, historyId(record), JSON.stringify({ schema_version: 1, record }))
      : entry.room.session.recordOwnedDownloadHistory(historyId(record), JSON.stringify({ schema_version: 1, record })));
    else track(owner, store, store.ready.then(() => {
      if (records.get(record.id) === record && !store.removing.has(historyId(record))) return store.reader.update(historyId(record), JSON.stringify({ schema_version: 1, record }));
    }));
  }
  async function ready(owner) { return storeFor(owner).ready; }
  async function flushHistory(owner) {
    const store = await ready(owner);
    while (store.writes.size) await Promise.all([...store.writes]);
    check(!store.error, store.error || "Native download history was not saved");
    await store.reader.flush();
  }
  function serialized(owner, operation) {
    const store = storeFor(owner), pending = store.actions.then(operation);
    store.actions = pending.catch(() => {}); return pending;
  }
  function publish(owner, record, previousStatus) {
    check(Number.isSafeInteger(++sequence), "Download lifecycle sequence exhausted");
    if (record) record.sequence = sequence;
    if (record && previousStatus !== record.status && ["complete", "failed"].includes(record.status)) record.unacknowledged = true;
    if (record) persistRecord(owner, record);
    emit({ type: "downloads", workerEnvironmentId: owner.workerEnvironmentId, formationPin: owner.formationPin, sequence });
  }
  function refresh(record, item) {
    const savePath = item.getSavePath();
    if (savePath && !record.artifact && record.modelReceipt?.status !== "imported") {
      check(path.isAbsolute(savePath), "Native download returned a relative save path"); record.path = savePath;
      if (record.userInitiated) record.filename = path.basename(savePath);
    }
    record.receivedBytes = item.getReceivedBytes(); record.totalBytes = Math.max(0, item.getTotalBytes()); record.updatedAtMs = Date.now();
  }
  function directory() {
    check(!preferencesError, preferencesError);
    const value = preferences.directory ?? electron.app.getPath("downloads");
    check(typeof value === "string" && path.isAbsolute(value), "Native Downloads directory is unavailable"); return value;
  }
  function reserveSavePath(filename) {
    const name = path.basename(filename.trim()), safe = !name || name === "." || name === ".." ? "download" : name;
    const parsed = path.parse(safe), root = directory();
    for (let index = 0; Number.isSafeInteger(index); ++index) {
      const candidate = path.join(root, index ? `${parsed.name} (${index})${parsed.ext}` : safe);
      if (!nativeFs.existsSync(candidate) && !activeSavePaths.has(candidate)) { activeSavePaths.add(candidate); return candidate; }
    }
    throw new Error("Native download destination sequence exhausted");
  }
  async function writePreferences(owner, changes, validateCurrent) {
    await validateCurrent();
    check(changes && typeof changes === "object" && !Array.isArray(changes) && Object.keys(changes).length > 0
      && Object.keys(changes).every(key => ["directory", "promptForLocation"].includes(key)), "Invalid native download preferences");
    if (Object.hasOwn(changes, "directory")) {
      check(changes.directory === null || typeof changes.directory === "string" && path.isAbsolute(changes.directory) && changes.directory.length <= 4096, "Download directory must be selected through the native folder picker");
      if (changes.directory !== null) { const stat = await fs.stat(changes.directory); check(stat.isDirectory(), "Selected download location is not a directory"); }
    }
    if (Object.hasOwn(changes, "promptForLocation")) check(typeof changes.promptForLocation === "boolean", "Download prompt preference must be a boolean");
    check(!preferencesError, preferencesError);
    const next = { ...preferences, ...changes }, temporary = path.join(hostRoot, `.download-preferences-${crypto.randomBytes(12).toString("hex")}`);
    let handle;
    try {
      handle = await fs.open(temporary, nativeFs.constants.O_CREAT | nativeFs.constants.O_EXCL | nativeFs.constants.O_WRONLY | nativeFs.constants.O_NOFOLLOW, 0o600);
      await handle.writeFile(JSON.stringify({ schema_version: 1, ...next }) + "\n"); await handle.sync(); await handle.close(); handle = null;
      await validateCurrent(); await fs.rename(temporary, preferencesFile);
      preferences = next; preferencesError = null;
      const parent = await fs.open(hostRoot, nativeFs.constants.O_RDONLY | nativeFs.constants.O_DIRECTORY | nativeFs.constants.O_NOFOLLOW);
      try { await parent.sync(); } finally { await parent.close(); }
      await validateCurrent(); publish(owner, null, null); return snapshot(owner);
    } finally { await handle?.close(); await fs.unlink(temporary).catch(error => { if (error.code !== "ENOENT") throw error; }); }
  }
  function setPreferences(owner, changes, validateCurrent) {
    const input = structuredClone(changes);
    const pending = preferenceWrites.then(() => writePreferences(owner, input, validateCurrent));
    preferenceWrites = pending.catch(() => {}); return pending;
  }
  function start(room, target, item, modelReceipt = null) {
    const owner = roomOwner(room); configureHistory(room);
    const downloadId = historyBackend ? historyBackend.identify(room, item) : item.getGUID();
    check(typeof downloadId === "string" && /^[a-f0-9]{8}(-[a-f0-9]{4}){3}-[a-f0-9]{12}$/i.test(downloadId), "Download history returned an invalid identity");
    const id = modelReceipt?.download_id ?? downloadId;
    check(!records.has(id), "Native download identity was already registered");
    let reservedPath = null, record;
    try {
      if (modelReceipt === null) {
        reservedPath = reserveSavePath(item.getFilename());
        if (preferences.promptForLocation) item.setSaveDialogOptions({ defaultPath: reservedPath });
        else room.session.setDownloadPath(directory());
      }
      record = { id, [historyBackend ? "standardId" : "nativeGuid"]: downloadId, scope: scope(owner), targetId: target.id, filename: item.getFilename(), path: reservedPath ?? "", url: item.getURL(), status: "started", receivedBytes: item.getReceivedBytes(), totalBytes: item.getTotalBytes(), startedAtMs: item.getStartTime() * 1000, updatedAtMs: Date.now(), userInitiated: modelReceipt === null, ...(modelReceipt ? { modelReceipt: structuredClone(modelReceipt) } : {}), unacknowledged: false, sequence: 0 };
      record.filename = path.basename(record.filename); refresh(record, item);
    } catch (error) { activeSavePaths.delete(reservedPath); throw error; }
    records.set(record.id, record); live.set(record.id, { item, room, reservedPath }); publish(owner, record, null);
    const update = status => {
      if (!live.has(record.id)) return;
      const previous = record.status;
      try { const oldPath = record.path; refresh(record, item);
        if (record.userInitiated && record.path !== oldPath) { activeSavePaths.delete(oldPath); activeSavePaths.add(record.path); } record.status = status; publish(owner, record, previous); }
      catch (error) { historyFailure(owner, storeFor(owner), error); }
    };
    const finish = status => {
      if (!live.has(record.id)) return;
      update(!record.userInitiated && status === "complete" ? "in_progress" : status); live.delete(record.id); activeSavePaths.delete(reservedPath); activeSavePaths.delete(record.path);
    };
    item.on("updated", (_event, state) => {
      if (state === "interrupted") {
        if (item.canResume()) update("paused"); else finish("failed");
      } else update(item.isPaused() ? "paused" : "in_progress");
    });
    item.once("done", (_event, state) => finish(state === "completed" ? "complete" : state === "cancelled" ? "canceled" : "failed"));
    item.once("destroyed", () => {
      if (!live.has(record.id)) return;
      const previous = record.status; live.delete(record.id); activeSavePaths.delete(reservedPath); activeSavePaths.delete(record.path); record.status = "unavailable"; record.updatedAtMs = Date.now();
      publish(owner, record, previous);
    });
  }
  function observeModel(room, receipt, item) {
    const owner = roomOwner(room);
    if (!item) return;
    if (!records.has(receipt.download_id)) {
      if (modelItems.has(item)) return;
      start(room, { id: receipt.target_id }, item, receipt); modelItems.add(item);
    }
    const record = records.get(receipt.download_id);
    check(belongs(owner, record) && !record.userInitiated && record.targetId === receipt.target_id, "Model download history changed owner");
    const previous = record.status;
    record.modelReceipt = structuredClone(receipt);
    if (receipt.status === "imported" && !record.artifact) record.path = "";
    if (["completed", "imported"].includes(receipt.status)) record.status = "complete";
    else if (receipt.status === "cancelled") record.status = "canceled";
    else if (["failed", "interrupted", "expired"].includes(receipt.status)) record.status = "failed";
    else if (receipt.status === "paused" || item.isPaused()) record.status = "paused";
    else record.status = "in_progress";
    record.receivedBytes = receipt.bytes; record.updatedAtMs = Date.now();
    publish(owner, record, previous);
  }
  async function verifiedModelFile(record, filename) {
    const proof = record.modelReceipt;
    check(proof && ["completed", "imported"].includes(proof.status) && proof.native_event?.state === "completed"
      && /^[a-f0-9]{64}$/.test(proof.sha256 || "") && Number.isSafeInteger(proof.bytes) && proof.bytes >= 0
      && proof.bytes <= proof.max_bytes, "Native download has no verified completed bytes");
    check(typeof filename === "string" && path.isAbsolute(filename) && filename.length <= 4096 && await fs.realpath(filename) === filename, "Imported download path is not canonical");
    const file = await fs.open(filename, nativeFs.constants.O_RDONLY | nativeFs.constants.O_NOFOLLOW);
    try {
      const before = await file.stat();
      check(before.isFile() && before.uid === process.getuid() && before.size === proof.bytes, "Imported download size or owner differs");
      const hash = crypto.createHash("sha256");
      for await (const bytes of file.createReadStream({ autoClose: false })) hash.update(bytes);
      const after = await file.stat();
      check(before.size === after.size && before.mtimeMs === after.mtimeMs && hash.digest("hex") === proof.sha256, "Imported download bytes differ from the native terminal item");
    } finally { await file.close(); }
  }
  async function acceptArtifact(owner, body, validateCurrent) {
    await validateCurrent();
    await flushHistory(owner);
    const record = records.get(body?.download_id), proof = record?.modelReceipt;
    if (!record) return { status: "not_in_history" };
    check(belongs(owner, record) && !record.userInitiated && proof
      && body.worker_environment_id === owner.workerEnvironmentId && body.target_id === record.targetId
      && body.browser_context_id === proof.browser_context_id && body.host_generation === proof.host_generation
      && body.arm?.formation_pin === owner.formationPin && Object.keys(body.arm).length === Object.keys(proof.arm).length
      && Object.entries(proof.arm).every(([key, value]) => body.arm[key] === value)
      && body.status === "completed" && body.bytes === proof.bytes && body.sha256 === proof.sha256
      && body.max_bytes === proof.max_bytes && body.native_event?.state === "completed"
      && Number.isSafeInteger(body.source_movement_seq) && body.source_movement_seq > proof.arm.source_movement_seq,
      "Imported artifact does not match the original native download");
    const artifact = body.artifact;
    check(artifact?.label === "browser.download" && artifact.payload_path === body.path
      && /^[a-f0-9]{64}$/.test(artifact.payload_content_hash || "") && artifact.payload_content_hash === body.content_hash,
      "Imported download has no exact artifact reference");
    if (record.artifact) {
      check(Object.keys(artifact).length === Object.keys(record.artifact).length && Object.entries(record.artifact).every(([key, value]) => artifact[key] === value), "Download already identifies another imported artifact"); return { status: "recorded" };
    }
    try { await verifiedModelFile(record, artifact.payload_path); }
    catch (error) { await validateCurrent(); if (record.artifactError !== error.message) { record.artifactError = error.message; publish(owner, record, record.status); } throw error; }
    await validateCurrent();
    check(records.get(record.id) === record, "Download history changed during artifact verification");
    record.artifact = { ...artifact }; record.path = artifact.payload_path; record.status = "complete"; record.artifactError = null;
    publish(owner, record, "complete");
    await flushHistory(owner); return { status: "recorded" };
  }
  function present(record) {
    const entry = live.get(record.id), item = entry?.room.active ? entry.item : null, paused = record.status === "paused";
    return { id: record.id, targetId: record.targetId, filename: record.filename, url: record.url, status: record.status,
      receivedBytes: record.receivedBytes, totalBytes: record.totalBytes, startedAtMs: record.startedAtMs, updatedAtMs: record.updatedAtMs,
      fileExists: !!record.path && nativeFs.existsSync(record.path), canCancel: !!item, canPause: !!item && !paused, canResume: !!item && paused && item.canResume(),
      userInitiated: record.userInitiated, artifactError: record.artifactError ?? null, artifactPending: !record.userInitiated && record.modelReceipt?.status === "imported" && !record.artifact, sequence: record.sequence };
  }
  function snapshot(owner) {
    const downloads = retainedRecords(owner).map(present);
    const unacknowledgedIds = downloads.filter(value => records.get(value.id).unacknowledged).map(value => value.id);
    return { sequence, downloads, unacknowledgedIds, preferences: { ...preferences }, error: stores.get(scope(owner))?.error || preferencesError,
      historyBackend: historyBackend?.api ?? "electron.ownedDownloadHistory.v1" };
  }
  async function search(owner, query) {
    check(query && typeof query === "object" && !Array.isArray(query) && Object.keys(query).sort().join(",") === "offset,text"
      && typeof query.text === "string" && Buffer.byteLength(query.text) <= HISTORY_BYTES
      && Number.isSafeInteger(query.offset) && query.offset >= 0 && query.offset <= 2147483547, "Invalid download history query");
    await flushHistory(owner); const store = await ready(owner);
    const result = await store.reader.search({ text: query.text, offset: query.offset, maxResults: 100 });
    check(result.text === query.text && result.offset === query.offset && typeof result.hasMore === "boolean" && Array.isArray(result.records)
      && result.records.length <= 100, "Native history query returned an invalid page");
    const downloads = result.records.map(row => {
      const native = nativeRecord(owner, row), record = records.get(native.id);
      check(record && historyId(record) === historyId(native) && belongs(owner, record), "Download history changed during its query");
      return present(record);
    });
    return { ...snapshot(owner), history: { text: query.text, offset: query.offset, hasMore: result.hasMore, downloads } };
  }
  async function reload(owner) {
    const store = await ready(owner), result = await store.reader.search({ text: "", offset: 0, maxResults: 0 });
    check(Array.isArray(result.records) && result.hasMore === false && result.offset === 0 && result.text === "", "Native history reload is incomplete");
    const remaining = new Set(result.records.map(row => historyId(nativeRecord(owner, row))));
    for (const [id, record] of records) if (belongs(owner, record) && !live.has(id) && !remaining.has(historyId(record))) records.delete(id);
  }
  async function action(owner, action, id, query, validateCurrent) {
    await validateCurrent(); await ready(owner); await validateCurrent();
    check(["snapshot", "search", "clear-history", "acknowledge", "open-folder", "open", "show-in-folder", "copy-address", "copy-path", "pause", "resume", "cancel", "remove"].includes(action), "Unknown download action");
    if (action === "search") { check(id === undefined, "Download history search does not take an item identity"); return search(owner, query); }
    check(query === undefined, "This download action does not take a history query");
    if (action === "clear-history") {
      check(id === undefined, "Clearing download history does not take an item identity");
      await flushHistory(owner); await validateCurrent();
      const store = await ready(owner), candidates = orderedRecords(owner).filter(record => !live.has(record.id) && FINISHED.has(record.status));
      for (const record of candidates) store.removing.add(historyId(record));
      try { await store.reader.clear(); await reload(owner); }
      finally { for (const record of candidates) store.removing.delete(historyId(record)); }
      for (const record of candidates) if (records.get(record.id) === record) persistRecord(owner, record);
      await validateCurrent(); publish(owner, null, null); return snapshot(owner);
    }
    if (["snapshot", "acknowledge", "open-folder"].includes(action)) {
      check(id === undefined, "This download action does not take an item identity");
      if (action === "acknowledge") {
        let changed = false;
        for (const record of records.values()) if (belongs(owner, record) && record.unacknowledged) { record.unacknowledged = false; changed = true; }
        if (changed) publish(owner, null, null);
      }
      if (action === "open-folder") { const error = await electron.shell.openPath(directory()); check(!error, error); }
      return snapshot(owner);
    }
    const record = records.get(id); check(record && belongs(owner, record), "Download is not available in this exact room");
    const entry = live.get(id), item = entry?.room.active ? entry.item : null;
    if (action === "open" || action === "show-in-folder") {
      check(record.path && nativeFs.existsSync(record.path), record.modelReceipt?.status === "imported" && !record.artifact ? "The downloaded file is being collected" : "The downloaded file is no longer available");
      if (!record.userInitiated) { const filename = record.path; await verifiedModelFile(record, filename); await validateCurrent(); check(records.get(id) === record && record.path === filename, "Download history changed before file presentation"); }
      if (action === "open") { check(record.status === "complete", "Download is not complete"); const error = await electron.shell.openPath(record.path); check(!error, error); }
      else electron.shell.showItemInFolder(record.path);
      record.unacknowledged = false; publish(owner, null, null);
    } else if (action === "copy-address") await electron.clipboard.writeText(record.url);
    else if (action === "copy-path") { check(!!record.path, "The native download destination is not available yet"); await electron.clipboard.writeText(record.path); }
    else if (action === "pause") { check(item && !item.isPaused(), "Download cannot be paused"); item.pause(); const previous = record.status; record.status = "paused"; publish(owner, record, previous); }
    else if (action === "resume") { check(item?.isPaused() && item.canResume(), "Download cannot resume"); item.resume(); const previous = record.status; record.status = "in_progress"; publish(owner, record, previous); }
    else if (action === "cancel") { check(item, "Download is no longer active"); item.cancel(); }
    else {
      check(!item && FINISHED.has(record.status), "Active download cannot be removed from history");
      await flushHistory(owner); await validateCurrent();
      const store = await ready(owner); store.removing.add(historyId(record));
      try { await store.reader.remove(historyId(record)); records.delete(id); }
      finally { store.removing.delete(historyId(record)); }
      publish(owner, null, null);
    }
    return snapshot(owner);
  }
  async function initialize() {
    let preferenceHandle;
    try {
      preferenceHandle = await fs.open(preferencesFile, nativeFs.constants.O_RDONLY | nativeFs.constants.O_NOFOLLOW);
      const stat = await preferenceHandle.stat();
      check(stat.isFile() && stat.uid === process.getuid() && (stat.mode & 0o777) === 0o600 && stat.size <= 16384, "Download preferences must be a bounded private regular file");
      const value = JSON.parse(await preferenceHandle.readFile("utf8"));
      check(Object.keys(value).sort().join(",") === "directory,promptForLocation,schema_version" && value.schema_version === 1 && typeof value.promptForLocation === "boolean"
        && (value.directory === null || typeof value.directory === "string" && path.isAbsolute(value.directory) && value.directory.length <= 4096), "Download preferences are invalid");
      preferences = { directory: value.directory, promptForLocation: value.promptForLocation };
    } catch (error) { if (error.code !== "ENOENT") preferencesError = `Download preferences are unavailable: ${error.message}`; }
    finally { await preferenceHandle?.close(); }
    if (historyBackend) historyBackend.validate();
    else check(typeof electron.session?.openOwnedDownloadHistory === "function", "Owned native download history engine is unavailable");
  }
  function releaseRoom(room) { for (const value of live.values()) if (value.room === room) value.item.cancel(); }
  async function stop() {
    for (const value of live.values()) value.item.cancel();
    await preferenceWrites;
    const results = await Promise.allSettled([...stores.values()].map(async store => {
      await store.actions;
      while (store.writes.size) await Promise.all([...store.writes]);
      try { await store.ready; await store.reader.flush(); } finally { store.reader?.close(); }
    }));
    const failed = results.find(result => result.status === "rejected"); if (failed) throw failed.reason;
  }
  return { initialize, start, observeModel, ready, acceptArtifact: (owner, body, validateCurrent) => serialized(owner, () => acceptArtifact(owner, body, validateCurrent)), snapshot, action: (owner, name, id, query, validateCurrent) => serialized(owner, () => action(owner, name, id, query, validateCurrent)), setPreferences, releaseRoom, stop };
}
module.exports = { createHumanDownloads };
