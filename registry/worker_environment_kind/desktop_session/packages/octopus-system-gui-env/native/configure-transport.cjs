'use strict';

const path = require('node:path');
const CONNECTION = 'desktop-host/connection.json';
const OPERATIONS = ['status', 'observe', 'screenshot', 'click', 'move', 'type', 'key', 'scroll', 'drag', 'control', 'cancel'];
const CONFIG_FIELDS = ['schema_version', 'action_url', 'viewer_url', 'auth_env_key', 'auth_header', 'auth_value_prefix', 'headers', 'request_timeout_ms', 'max_response_bytes', 'host', 'app_approval'];

function requireValue(condition, message) { if (!condition) throw new Error(message); }
function object(value, label) {
  requireValue(value !== null && typeof value === 'object' && !Array.isArray(value), `${label} must be an object`);
  return value;
}
function exactKeys(value, keys, label) {
  object(value, label);
  requireValue(Object.keys(value).every(key => keys.includes(key)), `${label} has unsupported fields`);
}
function relative(value) {
  return typeof value === 'string' && value.length > 0 && !value.includes('\\') && value.split('/').every(part => part && part !== '.' && part !== '..');
}
function authKey(config) { return config.host ? [] : config.auth_env_key == null ? [] : [config.auth_env_key]; }
function same(left, right) { return JSON.stringify(left) === JSON.stringify(right); }
function boundedText(value, maximum, nonempty = true) {
  return typeof value === 'string' && (!nonempty || value.length > 0)
    && Buffer.byteLength(value, 'utf8') <= maximum && !value.includes('\0');
}
function booleanFields(value, fields, label) {
  for (const field of fields) requireValue(typeof value[field] === 'boolean', `${label} ${field} must be boolean`);
}
function stringList(value, maximumItems, maximumBytes, label, unique = false) {
  requireValue(Array.isArray(value) && value.length <= maximumItems
    && Array.from(value).every(item => boundedText(item, maximumBytes)), `${label} requires bounded strings`);
  if (unique) requireValue(new Set(value).size === value.length, `${label} requires unique values`);
}
function validateApplicationProfile(profile) {
  const label = 'Desktop application profile';
  exactKeys(profile, ['full_ui', 'event_stream', 'informational', 'typing_interval_us',
    'focus_dom_field_by_click', 'mouse_targeting', 'suppress_menu_dismissal_on_window',
    'menu_bar_presentation', 'accessibility_presentation'], label);
  booleanFields(profile, ['focus_dom_field_by_click', 'suppress_menu_dismissal_on_window'], label);
  requireValue(Number.isSafeInteger(profile.typing_interval_us) && profile.typing_interval_us >= 0
    && profile.typing_interval_us <= 1000000, `${label} typing interval is invalid`);
  requireValue(['native_hit', 'supplied_window'].includes(profile.mouse_targeting), `${label} mouse targeting is invalid`);
  for (const field of ['full_ui', 'event_stream', 'informational']) {
    const options = profile[field];
    exactKeys(options, ['attribute', 'enforceFocusBeforeEnhanced', 'universalAccess', 'handshake'], `${label} ${field}`);
    booleanFields(options, ['enforceFocusBeforeEnhanced'], `${label} ${field}`);
    requireValue(['none', 'manualAccessibility', 'enhancedUserInterface', 'manualIfSupportedOtherwiseEnhanced'].includes(options.attribute)
      && ['none', 'untilRelease', 'pulse100ms'].includes(options.universalAccess)
      && ['none', 'nativeWindow'].includes(options.handshake), `${label} accessibility options are invalid`);
    requireValue(!options.enforceFocusBeforeEnhanced
      || ['enhancedUserInterface', 'manualIfSupportedOtherwiseEnhanced'].includes(options.attribute), `${label} focus requires enhanced accessibility`);
  }
  const menu = profile.menu_bar_presentation;
  exactKeys(menu, ['omit_first_item', 'excluded_title_substrings', 'clear_immediate_actions'], `${label} menu`);
  booleanFields(menu, ['omit_first_item', 'clear_immediate_actions'], `${label} menu`);
  stringList(menu.excluded_title_substrings, 16, 2048, `${label} menu filters`, true);
  const presentation = profile.accessibility_presentation;
  exactKeys(presentation, ['context_type', 'hide_cancel_actions', 'omit_calendar_event_children',
    'excluded_action_descriptions', 'capture_note'], `${label} presentation`);
  booleanFields(presentation, ['hide_cancel_actions', 'omit_calendar_event_children'], `${label} presentation`);
  requireValue(presentation.context_type === 'full_ui', `${label} presentation context is invalid`);
  stringList(presentation.excluded_action_descriptions, 128, 2048, `${label} action descriptions`);
  if (presentation.capture_note != null) requireValue(boundedText(presentation.capture_note, 32768, false), `${label} capture note is invalid`);
}
function validateApplicationBehavior(behavior) {
  const label = 'Desktop application behavior';
  exactKeys(behavior, ['schema_version', 'defaults', 'profiles', 'instruction_resource'], label);
  requireValue(behavior.schema_version === 1 && Array.isArray(behavior.profiles)
    && behavior.profiles.length <= 64 && Buffer.byteLength(JSON.stringify(behavior), 'utf8') <= 65536, `${label} requires a bounded version 1 profile set`);
  const resource = behavior.instruction_resource;
  exactKeys(resource, ['path', 'content_sha256'], `${label} instructions`);
  requireValue(relative(resource.path) && !resource.path.includes('\0')
    && typeof resource.content_sha256 === 'string' && /^[a-f0-9]{64}$/.test(resource.content_sha256), `${label} instructions require an exact package path and SHA-256`);
  validateApplicationProfile(behavior.defaults);
  const selectors = new Set();
  for (const selected of behavior.profiles) {
    exactKeys(selected, ['selector', 'behavior'], `${label} selected profile`);
    exactKeys(selected.selector, ['match', 'bundle_id'], `${label} selector`);
    const { match, bundle_id: bundle } = selected.selector;
    requireValue(['exact', 'prefix'].includes(match) && typeof bundle === 'string'
      && /^[A-Za-z0-9._-]+$/.test(bundle) && bundle.length <= 255
      && !selectors.has(`${match}:${bundle}`), `${label} selector is invalid or repeated`);
    selectors.add(`${match}:${bundle}`);
    validateApplicationProfile(selected.behavior);
  }
}
function validateApplicationPolicy(policy) {
  const label = 'Desktop application policy';
  exactKeys(policy, ['schema_version', 'enabled', 'allow_persistent_approval', 'required_access',
    'configured_access', 'forbidden_bundle_ids', 'elevated_risk_bundle_ids',
    'high_risk_url_schemes', 'high_risk_warning_subtitle'], label);
  booleanFields(policy, ['enabled', 'allow_persistent_approval'], label);
  const exact = value => boundedText(value, 4096) && value.trim() === value;
  requireValue(policy.schema_version === 1 && exact(policy.high_risk_warning_subtitle)
    && Buffer.byteLength(JSON.stringify(policy), 'utf8') <= 65536, `${label} declared values are invalid`);
  for (const field of ['required_access', 'configured_access']) {
    const access = policy[field];
    exactKeys(access, ['default_access', 'bundle_ids'], `${label} ${field}`);
    object(access.bundle_ids, `${label} ${field} bundle ids`);
    const entries = Object.entries(access.bundle_ids);
    requireValue(['allow', 'deny'].includes(access.default_access) && entries.length <= 4096
      && entries.every(([bundle, grant]) => exact(bundle) && ['allow', 'deny'].includes(grant)), `${label} access decisions are invalid`);
  }
  for (const field of ['forbidden_bundle_ids', 'elevated_risk_bundle_ids', 'high_risk_url_schemes']) {
    stringList(policy[field], 4096, 4096, `${label} ${field}`, true);
    requireValue(policy[field].every(exact), `${label} ${field} requires exact values`);
  }
}
function validateConfig(config) {
  exactKeys(config, CONFIG_FIELDS, 'Desktop config');
  requireValue(config.schema_version === 1, 'Desktop config schema must be 1');
  for (const [field, protocols] of [['action_url', ['http:', 'https:']], ['viewer_url', ['http:', 'https:', 'ws:', 'wss:']]]) {
    const value = config[field];
    requireValue(typeof value === 'string' && value.trim() === value, `Desktop ${field} must be an unpadded URL`);
    const url = new URL(value);
    requireValue(protocols.includes(url.protocol) && url.hostname && !url.username && !url.password && !url.search && !url.hash, `Desktop ${field} has an unsupported or secret-bearing URL`);
    const loopback = url.hostname === 'localhost' || url.hostname === '[::1]' || /^127\.\d+\.\d+\.\d+$/.test(url.hostname);
    requireValue(!['http:', 'ws:'].includes(url.protocol) || loopback, `Desktop ${field} requires TLS outside loopback`);
  }
  if (config.auth_env_key != null) requireValue(typeof config.auth_env_key === 'string' && /^[A-Za-z_][A-Za-z0-9_]*$/.test(config.auth_env_key), 'Desktop auth_env_key is invalid');
  const header = config.auth_header ?? 'Authorization';
  const prefix = config.auth_value_prefix ?? 'Bearer ';
  requireValue(typeof header === 'string' && /^[A-Za-z0-9-]+$/.test(header), 'Desktop auth_header is invalid');
  requireValue(typeof prefix === 'string' && !/[\r\n]/.test(prefix), 'Desktop auth prefix is invalid');
  object(config.headers ?? {}, 'Desktop headers');
  for (const [name, value] of Object.entries(config.headers ?? {})) {
    requireValue(/^[A-Za-z0-9-]+$/.test(name) && !['authorization', 'proxy-authorization', 'cookie', 'set-cookie'].includes(name.toLowerCase()), 'Desktop headers cannot carry authentication values');
    requireValue(typeof value === 'string' && !/[\r\n]/.test(value), 'Desktop header value is invalid');
  }
  for (const [field, minimum, maximum] of [['request_timeout_ms', 1, 600000], ['max_response_bytes', 1024, 97871192]]) {
    if (config[field] !== undefined) requireValue(Number.isSafeInteger(config[field]) && config[field] >= minimum && config[field] <= maximum, `Desktop ${field} is outside its bounds`);
  }
  if (config.app_approval != null) {
    exactKeys(config.app_approval, ['operator_interface', 'resume_tool'], 'Desktop app approval');
    requireValue(config.host != null, 'Application tools require explicit native host configuration');
    for (const key of ['operator_interface', 'resume_tool']) {
      requireValue(typeof config.app_approval[key] === 'string' && /^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(config.app_approval[key]) && config.app_approval[key].length <= 2048, 'Desktop app approval identity is invalid');
    }
  }
  if (config.host != null) {
    exactKeys(config.host, ['native_program', 'native_sandbox_mode', 'native_sandbox_profile',
      'state_directory', 'device_lock', 'connection_file', 'application_behavior',
      'application_policy', 'application_preferences'], 'Desktop native host');
    validateApplicationBehavior(config.host.application_behavior);
    if (config.host.application_policy != null) validateApplicationPolicy(config.host.application_policy);
    if (config.host.application_preferences != null) {
      exactKeys(config.host.application_preferences, ['root_slot'], 'Desktop application preferences');
      const slot = config.host.application_preferences.root_slot;
      requireValue(typeof slot === 'string' && /^[A-Za-z0-9_]+$/.test(slot) && slot.length <= 128, 'Desktop application preferences require an explicit private root slot');
    }
    if (Object.hasOwn(config.host, 'native_sandbox_mode')) requireValue(['enforced', 'disabled'].includes(config.host.native_sandbox_mode), 'Desktop native sandbox mode is invalid');
    requireValue(relative(config.host.native_program) && relative(config.host.native_sandbox_profile), 'Desktop native files must be declared package members');
    exactKeys(config.host.device_lock, ['root_slot', 'file_name'], 'Desktop device lock');
    const lock = config.host.device_lock;
    requireValue(typeof lock.root_slot === 'string' && /^[A-Za-z0-9_]+$/.test(lock.root_slot) && lock.root_slot.length <= 128, 'Desktop device lock requires an explicit root slot');
    requireValue(typeof lock.file_name === 'string' && lock.file_name.length > 0 && lock.file_name.length <= 255 && path.posix.basename(lock.file_name) === lock.file_name && !['.', '..'].includes(lock.file_name) && !lock.file_name.includes('\0'), 'Desktop device lock file_name must be one plain file name inside its root slot');
    requireValue(config.host.connection_file === CONNECTION && config.host.state_directory === 'desktop-host', 'Desktop native private paths disagree');
    requireValue(config.action_url === 'http://127.0.0.1:0/v1/action' && config.viewer_url === 'http://127.0.0.1:0/view', 'Desktop native host requires port-zero placeholders');
    requireValue(config.auth_env_key == null && header === 'Authorization' && prefix === 'Bearer ' && Object.keys(config.headers ?? {}).length === 0, 'Desktop native authentication belongs to its private manifest');
  }
}

function validateInputs(process, config, tool) {
  object(process, 'Desktop process');
  requireValue(same(process.inherit_env ?? [], authKey(config)), 'Desktop process auth environment disagrees with its explicit transport');
  requireValue(Object.keys(process.env ?? {}).length === 0 && (process.inherit_env_from_interface ?? []).length === 0, 'Desktop process has undeclared transport environment inputs');
  if (tool) requireValue(same(process.private_read_subpaths ?? [], config.host ? [{ path: CONNECTION }] : []), 'Desktop tool private inputs disagree with its explicit transport');
}

function planDesktopTransport({ interfaceId, descriptor, currentConfig, config, life, tools, nativeHostProcess }) {
  requireValue(typeof interfaceId === 'string' && /^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(interfaceId), 'Desktop interface identity is invalid');
  requireValue(object(descriptor, 'Desktop descriptor').id === interfaceId && descriptor.interface_driver_ref === 'system_gui', 'Desktop descriptor is outside this door family');
  validateConfig(currentConfig);
  validateConfig(config);
  object(life, 'Desktop life');
  requireValue(Array.isArray(life.processes) && Array.isArray(tools), 'Desktop life and tool declarations are required');
  const basename = process => path.posix.basename(object(process, 'Desktop life process').bin ?? '');
  const doors = life.processes.filter(process => basename(process) === 'octopus-desktop-session-tool-door-life');
  const hosts = life.processes.filter(process => basename(process) === 'octopus-desktop-host');
  requireValue(doors.length === 1 && hosts.length === (currentConfig.host ? 1 : 0), 'Desktop life does not match its current transport');
  validateInputs(doors[0], currentConfig, false);
  const operations = new Set();
  const ids = new Set();
  const nextTools = tools.map(tool => {
    object(tool, 'Desktop tool');
    const operation = tool.handler_config?.operation;
    requireValue(tool.interface === interfaceId && same(tool.supported_worker_environment_kinds, ['desktop_session']) && OPERATIONS.includes(operation), 'Tool does not belong to this exact desktop door');
    requireValue(typeof tool.id === 'string' && !ids.has(tool.id) && !operations.has(operation), 'Desktop tool identity or operation is repeated');
    requireValue(basename(tool.process) === 'octopus-gui-session-tool', 'Desktop tool executor is outside its owning family');
    ids.add(tool.id); operations.add(operation);
    validateInputs(tool.process, currentConfig, true);
    const next = structuredClone(tool);
    next.process.inherit_env = authKey(config);
    next.process.private_read_subpaths = config.host ? [{ path: CONNECTION }] : [];
    return next;
  });
  requireValue(operations.size === OPERATIONS.length, 'Desktop transport plan requires the complete declared tool surface');
  const nextLife = structuredClone(life);
  const door = nextLife.processes.find(process => basename(process) === 'octopus-desktop-session-tool-door-life');
  door.inherit_env = authKey(config);
  if (!config.host) {
    nextLife.processes = nextLife.processes.filter(process => basename(process) !== 'octopus-desktop-host');
  } else if (!hosts.length) {
    requireValue(nativeHostProcess && basename(nativeHostProcess) === 'octopus-desktop-host', 'Native transport requires its explicit native host process declaration');
    validateInputs(nativeHostProcess, config, false);
    requireValue(nativeHostProcess.authority?.allow_sandbox_delegation === true, 'Native host requires its declared sandbox delegate');
    nextLife.processes.push(structuredClone(nativeHostProcess));
  }
  return { interfaceId, desktop: structuredClone(config), life: nextLife, tools: nextTools };
}

module.exports = { planDesktopTransport };
