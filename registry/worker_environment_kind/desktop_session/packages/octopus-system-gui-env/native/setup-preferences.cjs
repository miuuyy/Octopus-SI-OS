'use strict';

const fs = require('node:fs/promises');
const { constants } = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const os = require('node:os');

const SLOT = 'desktop_app_preferences_root';
const DEVICE_LOCK_SLOT = 'desktop_device_lock_root';
const MAX_HOST_BYTES = 1024 * 1024;
const digest = bytes => crypto.createHash('sha256').update(bytes).digest('hex');

function needsDesktopApplicationPreferences(plannedWrites) {
  if (!Array.isArray(plannedWrites)) throw new Error('Desktop preference setup requires a file write plan');
  return plannedWrites.some(write => {
    if (path.basename(write.destination) !== 'desktop.json' || typeof write.content !== 'string') return false;
    const config = JSON.parse(write.content);
    const preferences = config.host?.application_preferences;
    if (preferences === undefined || preferences === null) return false;
    if (!preferences || typeof preferences !== 'object' || Array.isArray(preferences)
        || Object.keys(preferences).length !== 1 || preferences.root_slot !== SLOT) {
      throw new Error('Desktop preference setup requires the declared application preference root slot');
    }
    return true;
  });
}

async function directoryState(directory, privateMode) {
  const stat = await fs.lstat(directory);
  if (!stat.isDirectory() || stat.isSymbolicLink() || stat.uid !== process.getuid()
      || (privateMode && (stat.mode & 0o777) !== 0o700)
      || await fs.realpath(directory) !== directory) {
    throw new Error('Desktop preference setup requires an owned canonical directory');
  }
  return stat;
}

async function readHost(hostPath) {
  const handle = await fs.open(hostPath, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const stat = await handle.stat();
    if (!stat.isFile() || stat.nlink !== 1 || stat.uid !== process.getuid() || stat.size > MAX_HOST_BYTES) {
      throw new Error('Desktop preference setup requires an owned bounded host manifest');
    }
    const bytes = await handle.readFile();
    if (bytes.length > MAX_HOST_BYTES) throw new Error('Desktop preference host manifest exceeded its byte limit');
    return bytes;
  } finally { await handle.close(); }
}

function hostValue(bytes) {
  const value = JSON.parse(bytes.toString('utf8'));
  if (!value || value.host_manifest_version !== 1 || !value.root_refs
      || typeof value.root_refs !== 'object' || Array.isArray(value.root_refs)
      || Object.keys(value).some(key => !['host_manifest_version', 'root_refs'].includes(key))) {
    throw new Error('Desktop preference setup requires the canonical host manifest schema');
  }
  for (const root of Object.values(value.root_refs)) {
    if (typeof root !== 'string' || !path.isAbsolute(root) || root.includes('\0')) {
      throw new Error('Desktop preference setup found an invalid host root');
    }
  }
  return value;
}

async function planDesktopApplicationPreferences({ octopusHomeRoot, plannedWrites }) {
  const created = [];
  async function rollback() {
    for (const entry of [...created].reverse()) {
      try {
        const stat = await fs.lstat(entry.path);
        if (!stat.isDirectory() || stat.isSymbolicLink() || stat.dev !== entry.dev || stat.ino !== entry.ino) continue;
        await fs.rmdir(entry.path);
      } catch (error) {
        if (!['ENOENT', 'ENOTEMPTY', 'EEXIST'].includes(error.code)) throw error;
      }
    }
  }
  if (!needsDesktopApplicationPreferences(plannedWrites)) return { writes: [], rollback };
  try {
    if (typeof octopusHomeRoot !== 'string' || !path.isAbsolute(octopusHomeRoot)
        || path.normalize(octopusHomeRoot) !== octopusHomeRoot) {
      throw new Error('Desktop preference setup requires an absolute canonical Home');
    }
    await directoryState(octopusHomeRoot, false);
    const hostPath = path.join(octopusHomeRoot, 'host.json');
    const before = await readHost(hostPath);
    const beforeDigest = digest(before);
    hostValue(before);
    const prior = plannedWrites.filter(write => write.destination === hostPath);
    if (prior.length > 1) throw new Error('Desktop preference setup found duplicate host manifest writes');
    let effective = before;
    if (prior.length) {
      const write = prior[0];
      if (typeof write.content !== 'string' || write.publishBarrier !== false
          || write.expectedBeforeDigest !== beforeDigest
          || Buffer.byteLength(write.content) > MAX_HOST_BYTES
          || digest(Buffer.from(write.content)) !== write.afterDigest) {
        throw new Error('Desktop preference setup found an unverified planned host manifest');
      }
      effective = Buffer.from(write.content);
    }
    const host = hostValue(effective);
    const client = path.join(octopusHomeRoot, 'client');
    const preferences = path.join(client, 'desktop-app-preferences');
    if (Object.hasOwn(host.root_refs, SLOT) && host.root_refs[SLOT] !== preferences) {
      throw new Error('Desktop application preferences already have a different explicit root; setup cannot relocate them');
    }
    for (const directory of [client, preferences]) {
      try { await directoryState(directory, directory === preferences); }
      catch (error) {
        if (error.code !== 'ENOENT') throw error;
        await fs.mkdir(directory, { mode: 0o700 });
        const stat = await directoryState(directory, true);
        created.push({ path: directory, dev: stat.dev, ino: stat.ino });
      }
    }
    host.root_refs[SLOT] = preferences;
    const userSupport = path.join(os.homedir(), 'Library', 'Application Support');
    await directoryState(userSupport, false);
    const deviceLockParent = path.join(userSupport, 'Octopus');
    const deviceLock = path.join(deviceLockParent, 'desktop-device');
    if (Object.hasOwn(host.root_refs, DEVICE_LOCK_SLOT) && host.root_refs[DEVICE_LOCK_SLOT] !== deviceLock) {
      throw new Error('The desktop device lock already has a different explicit root; setup cannot relocate it');
    }
    for (const directory of [deviceLockParent, deviceLock]) {
      try { await directoryState(directory, directory === deviceLock); }
      catch (error) {
        if (error.code !== 'ENOENT') throw error;
        await fs.mkdir(directory, { mode: 0o700 });
        const stat = await directoryState(directory, true);
        created.push({ path: directory, dev: stat.dev, ino: stat.ino });
      }
    }
    host.root_refs[DEVICE_LOCK_SLOT] = deviceLock;
    const content = `${JSON.stringify(host, null, 2)}\n`;
    if (Buffer.byteLength(content) > MAX_HOST_BYTES) throw new Error('Desktop preference host manifest exceeds its byte limit');
    return {
      writes: [{ destination: hostPath, expectedBeforeDigest: beforeDigest, afterDigest: digest(Buffer.from(content)),
        content, tag: 'desktop-app-preferences-root', publishBarrier: false }],
      rollback,
    };
  } catch (error) {
    await rollback();
    throw error;
  }
}

async function readComputerUsePreference(root) {
  await directoryState(root, true);
  let handle;
  try { handle = await fs.open(path.join(root, 'computer-use.json'), constants.O_RDONLY | constants.O_NOFOLLOW); }
  catch (error) { if (error.code === 'ENOENT') return { schema_version: 1, enabled: false }; throw error; }
  try {
    const stat = await handle.stat();
    if (!stat.isFile() || stat.nlink !== 1 || stat.uid !== process.getuid() || (stat.mode & 0o777) !== 0o600 || stat.size > 1024) throw new Error('Computer Use preference must be an owned private file');
    const bytes = await handle.readFile();
    if (bytes.length > 1024) throw new Error('Computer Use preference exceeded its bound');
    const value = JSON.parse(bytes.toString('utf8'));
    if (!value || typeof value !== 'object' || Array.isArray(value) || Object.keys(value).length !== 2
        || value.schema_version !== 1 || typeof value.enabled !== 'boolean') throw new Error('Computer Use preference is malformed');
    return value;
  } finally { await handle.close(); }
}

async function writeComputerUsePreference(root, enabled) {
  if (typeof enabled !== 'boolean') throw new Error('Computer Use selection requires an explicit Boolean');
  await readComputerUsePreference(root);
  const temporary = path.join(root, `.computer-use-${crypto.randomBytes(16).toString('hex')}.json`);
  const handle = await fs.open(temporary, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
  try {
    await handle.writeFile(JSON.stringify({ schema_version: 1, enabled }) + '\n'); await handle.sync();
  } finally { await handle.close(); }
  try {
    await directoryState(root, true); await fs.rename(temporary, path.join(root, 'computer-use.json'));
    const directory = await fs.open(root, constants.O_RDONLY | constants.O_DIRECTORY | constants.O_NOFOLLOW);
    try { await directory.sync(); } finally { await directory.close(); }
    return await readComputerUsePreference(root);
  } finally { await fs.rm(temporary, { force: true }); }
}

module.exports = { planDesktopApplicationPreferences, needsDesktopApplicationPreferences, readComputerUsePreference, writeComputerUsePreference };
