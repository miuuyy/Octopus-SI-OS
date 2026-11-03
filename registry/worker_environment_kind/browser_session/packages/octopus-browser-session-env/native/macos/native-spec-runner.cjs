"use strict";
const { app } = require('electron');
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { createRequire } = require('node:module');
const [sourceRoot, resultPath, expectedSpec, inventorySha, ...extra] = process.argv.slice(2);
const hash = file => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
if (extra.length || !path.isAbsolute(sourceRoot || '') || fs.realpathSync(sourceRoot) !== sourceRoot
  || !path.isAbsolute(resultPath || '') || !/^[a-f0-9]{64}$/.test(expectedSpec || '') || !/^[a-f0-9]{64}$/.test(inventorySha || '')) throw new Error('Owned native spec runner requires exact source and evidence paths');
const spec = path.join(sourceRoot, 'electron/spec/api-owned-guest-spec.ts');
if (hash(spec) !== expectedSpec || process.versions.electron !== '45.2.1') throw new Error('Owned native spec source or Electron version differs');
for (const name of ['enable-features', 'disable-features', 'no-sandbox', 'disable-gpu']) if (app.commandLine.hasSwitch(name)) throw new Error('Owned native specs cannot use runtime feature or sandbox switches');
const profile = path.join(path.dirname(resultPath), 'native-spec-profile');
fs.mkdirSync(profile, { mode: 0o700 }); app.setPath('userData', profile);
app.on('window-all-closed', () => {});
const fatal = error => { process.stderr.write(String(error?.stack || error) + '\n'); app.exit(1); };
process.on('uncaughtException', fatal); process.on('unhandledRejection', fatal);
app.whenReady().then(() => {
  process.env.TS_NODE_PROJECT = path.join(sourceRoot, 'electron/tsconfig.spec.json');
  createRequire(path.join(sourceRoot, 'electron/package.json'))('ts-node/register');
  const Mocha = createRequire(path.join(sourceRoot, 'electron/spec/package.json'))('mocha');
  const passes = [], failures = [], pending = [];
  const row = test => ({ fullTitle: test.fullTitle(), duration: test.duration });
  class EvidenceReporter {
    constructor(runner) {
      runner.on('pass', test => passes.push(row(test)));
      runner.on('pending', test => pending.push(row(test)));
      runner.on('fail', (test, error) => failures.push({ ...row(test), error: String(error?.stack || error) }));
    }
  }
  const mocha = new Mocha({ reporter: EvidenceReporter, forbidOnly: true, forbidPending: true, retries: 0, timeout: 30000 });
  mocha.addFile(spec);
  const runner = mocha.run(count => {
    if (hash(spec) !== expectedSpec) return fatal(new Error('Native spec source changed during execution'));
    fs.writeFileSync(resultPath, JSON.stringify({ schema_version: 1,
      engine: { inventory_sha256: inventorySha, executable: fs.realpathSync(process.execPath), electron_version: process.versions.electron, node_version: process.versions.node },
      spec_source_sha256: expectedSpec, stats: runner.stats, passes, failures, pending }, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
    app.exit(count === 0 && failures.length === 0 && pending.length === 0 ? 0 : 1);
  });
}).catch(fatal);
