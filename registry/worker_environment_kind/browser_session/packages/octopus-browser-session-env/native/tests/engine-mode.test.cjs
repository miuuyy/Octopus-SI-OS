'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { engineInfo, packagingMode, sealBrowserEngine, verifyBrowserEngine } = require('../engine-mode.cjs');

test('engine selection is explicit and never repairs a missing owned artifact with standard Electron', () => {
  assert.equal(packagingMode([], {}), 'standard');
  assert.equal(packagingMode(['--browser-engine=standard'], {}), 'standard');
  assert.equal(packagingMode(['--browser-engine=owned'], { OCTOPUS_OWNED_ELECTRON_RECEIPT: '/accepted/owned-engine.json' }), 'owned');
  assert.equal(packagingMode([], { OCTOPUS_BROWSER_ENGINE: 'owned', OCTOPUS_OWNED_ELECTRON_RECEIPT: '/accepted/owned-engine.json' }), 'owned');
  assert.throws(() => packagingMode(['--browser-engine=owned'], {}), /requires OCTOPUS_OWNED/);
  assert.throws(() => packagingMode([], { OCTOPUS_OWNED_ELECTRON_RECEIPT: '/accepted/owned-engine.json' }), /cannot consume/);
  assert.throws(() => packagingMode(['--browser-engine=standard'], { OCTOPUS_BROWSER_ENGINE: 'owned' }), /disagree/);
  assert.throws(() => packagingMode(['--browser-engine=unknown'], {}), /Usage/);
  assert.throws(() => engineInfo('auto'), /Unknown/);
  assert.throws(() => verifyBrowserEngine({ isPackaged: false }), /explicit packaged receipt/);
  assert.equal(engineInfo('owned').capabilities.nativeNavigationHistory, true);
  assert.equal(engineInfo('standard').capabilities.nativeNavigationHistory, false);
  assert.equal(engineInfo('standard').capabilities.persistentDownloadHistory, true);
  assert.equal(engineInfo('standard').presentationProtocol, 'electron.webContentsView.v1');
  assert.equal(engineInfo('standard').capabilities.scaledPresentation, false);
  assert.throws(() => { engineInfo('standard').capabilities.nativeNavigationHistory = true; }, TypeError);
});

test('standard receipt pins the selected engine bytes and rejects mode or binary drift', t => {
  const root = fs.realpathSync(fs.mkdtempSync('/tmp/octopus-engine-receipt-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const bundleRoot = path.join(root, 'Fixture.app'), resourcesRoot = path.join(bundleRoot, 'Contents/Resources');
  const executablePath = path.join(bundleRoot, 'Contents/MacOS/Octopus');
  const framework = path.join(bundleRoot, 'Contents/Frameworks/Electron Framework.framework/Versions/A/Electron Framework');
  for (const directory of [resourcesRoot, path.dirname(executablePath), path.dirname(framework)]) fs.mkdirSync(directory, { recursive: true });
  fs.writeFileSync(executablePath, 'test fixture; never executed'); fs.writeFileSync(framework, 'test engine bytes; never loaded');
  const options = { resourcesRoot, executablePath, isPackaged: true, architecture: 'arm64', electronVersion: '45.2.1' };
  assert.throws(() => verifyBrowserEngine(options), /ENOENT/);
  const receipt = sealBrowserEngine({ mode: 'standard', bundleRoot, architecture: options.architecture, electronVersion: options.electronVersion });
  assert.equal(verifyBrowserEngine(options).mode, 'standard'); assert.equal(verifyBrowserEngine(options).engineProof, null);
  assert.throws(() => verifyBrowserEngine({ ...options, electronVersion: '42.0.0' }), /identity differs/);
  const receiptPath = path.join(resourcesRoot, 'browser-engine.json');
  fs.writeFileSync(receiptPath, JSON.stringify({ ...receipt, mode: 'owned' }));
  assert.throws(() => verifyBrowserEngine(options), /identity differs/);
  fs.writeFileSync(receiptPath, JSON.stringify(receipt)); fs.writeFileSync(framework, 'changed bytes');
  assert.throws(() => verifyBrowserEngine(options), /differs from the packaged inventory/);
});
