"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const { createRequire } = require("node:module");
const net = require("node:net");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");
const tls = require("node:tls");
const { TEST_CA_CERT, TEST_LEAF_CERT, TEST_LEAF_KEY } = require("./tls_fixture.js");

const INTERFACE = path.resolve(__dirname, "..");
const PASSWORD = "test-app-password";
const descriptor = JSON.parse(fs.readFileSync(path.join(INTERFACE, "interface.json"), "utf8"));
const names = descriptor.interface_driver_config.private_config.environment;
const connectTls = tls.connect;
tls.connect = (options, ...rest) => connectTls({ ...options, ca: TEST_CA_CERT }, ...rest);
const roots = [];

function assertOperationCatalog(root, prefix, count) {
  const Ajv = createRequire(path.resolve(root, "../../../app/package.json"))("ajv");
  const ajv = new Ajv({ strict: true, strictTypes: true, allErrors: true });
  const config = JSON.parse(fs.readFileSync(path.join(root, "interface.json"), "utf8")).interface_driver_config.private_config;
  assert.deepEqual([config.operation_catalog_dir, config.operation_catalog_label, config.operation_request_label], ["operations", `${prefix}.operation_catalog`, `${prefix}.operation.request`]);
  const files = fs.readdirSync(path.join(root, "operations")).sort();
  assert.equal(files.length, count);
  for (const file of files) {
    const operation = JSON.parse(fs.readFileSync(path.join(root, "operations", file), "utf8"));
    assert.deepEqual(Object.keys(operation).sort(), ["description", "handler_config", "id", "input_schema", "output_label", "request_label"]);
    assert.equal(`${operation.id}.json`, file);
    assert.match(operation.id, new RegExp(`^${prefix}\\.[a-z_]+$`));
    assert.deepEqual([operation.request_label, operation.output_label], [`${prefix}.operation.request`, `${prefix}.operation.result`]);
    assert.deepEqual([operation.input_schema.type, operation.input_schema.additionalProperties], ["object", false]);
    ajv.compile(operation.input_schema);
  }
  assert.equal(JSON.parse(fs.readFileSync(path.join(root, "application.json"), "utf8")).task_capabilities, undefined);
  const contract = JSON.parse(fs.readFileSync(path.resolve(root, `../../artifact/artifact-application-${prefix}-context/artifact.json`), "utf8")).text;
  assert.doesNotMatch(contract, /artifact\.send|delivery\.result|\.failure|\.operation\b/);
}

test.after(() => {
  for (const root of roots) fs.rmSync(root, { recursive: true, force: true });
});

function listen(server) {
  return new Promise((resolve) => server.listen(0, "127.0.0.1", () => {
    server.unref();
    resolve({ port: server.address().port, close: () => new Promise((done) => server.close(done)) });
  }));
}

function lines(socket, onLine) {
  let buffer = "";
  const onData = (chunk) => {
    buffer += chunk;
    for (let index = buffer.indexOf("\r\n"); index >= 0; index = buffer.indexOf("\r\n")) {
      const line = buffer.slice(0, index);
      buffer = buffer.slice(index + 2);
      onLine(line);
    }
  };
  socket.on("data", onData);
  socket.on("error", () => {});
  return onData;
}

function secret(token) {
  return Buffer.from(token, "base64").toString("utf8").split("\0")[2];
}

function imapServer(refusal = "NO [AUTHENTICATIONFAILED] Invalid credentials") {
  return listen(tls.createServer({ cert: TEST_LEAF_CERT, key: TEST_LEAF_KEY }, (socket) => {
    let signingIn = null;
    lines(socket, (line) => {
      const [tag, command] = line.split(" ");
      if (signingIn) {
        socket.write(secret(line) === PASSWORD ? `${signingIn} OK signed in\r\n` : `${signingIn} ${refusal}\r\n`);
        signingIn = null;
      } else if (command === "AUTHENTICATE") {
        signingIn = tag;
        socket.write("+ \r\n");
      } else if (command === "LOGOUT") {
        socket.end(`* BYE\r\n${tag} OK bye\r\n`);
      } else {
        socket.write(`${tag} BAD unknown\r\n`);
      }
    });
    socket.write("* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN] ready\r\n");
  }));
}

function smtpServer(offerStarttls = true) {
  const seen = { plaintextAuth: false };
  const server = net.createServer((plain) => {
    let socket = plain;
    const handle = (line) => {
      const verb = line.split(" ")[0].toUpperCase();
      const secure = socket !== plain;
      if (verb === "EHLO") {
        socket.write(secure ? "250-fake\r\n250 AUTH PLAIN LOGIN\r\n" : `250-fake\r\n250 ${offerStarttls ? "STARTTLS" : "AUTH PLAIN LOGIN"}\r\n`);
      } else if (verb === "STARTTLS" && offerStarttls && !secure) {
        plain.removeListener("data", onPlain);
        plain.write("220 go ahead\r\n");
        socket = new tls.TLSSocket(plain, { isServer: true, cert: TEST_LEAF_CERT, key: TEST_LEAF_KEY });
        lines(socket, handle);
      } else if (verb === "AUTH") {
        seen.plaintextAuth ||= !secure;
        socket.write(secret(line.split(" ")[2]) === PASSWORD ? "235 2.7.0 Accepted\r\n" : "535 5.7.8 Username and Password not accepted\r\n");
      } else if (verb === "QUIT") {
        socket.end("221 bye\r\n");
      } else {
        socket.write("502 unknown\r\n");
      }
    };
    const onPlain = lines(plain, handle);
    plain.write("220 fake ESMTP\r\n");
  });
  return listen(server).then((listening) => ({ ...listening, seen }));
}

async function closedPort() {
  const { port, close } = await listen(net.createServer());
  await close();
  return port;
}

function connectionModule(configure) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "octopus-email-connection-"));
  roots.push(root);
  fs.mkdirSync(path.join(root, "packages"));
  fs.copyFileSync(path.join(INTERFACE, "packages", "connection.cjs"), path.join(root, "packages", "connection.cjs"));
  const config = JSON.parse(fs.readFileSync(path.join(INTERFACE, "config.json"), "utf8"));
  configure(config);
  fs.writeFileSync(path.join(root, "config.json"), JSON.stringify(config));
  return require(path.join(root, "packages", "connection.cjs"));
}

function context({ input = {}, values = {} } = {}) {
  const saved = [];
  const progress = [];
  return {
    saved,
    progress,
    ctx: {
      descriptor,
      interfaceId: "world_email",
      values,
      input,
      saveValues: async (changes) => { saved.push(changes); },
      privateDirectory: os.tmpdir(),
      launchEnvironment: () => undefined,
      openExternal: async () => {},
      progress: (update) => progress.push(update.message),
      signal: new AbortController().signal,
      fetch: globalThis.fetch,
    },
  };
}

test("connect signs in to both servers, finds the submission port and saves what worked", async () => {
  const imap = await imapServer();
  const smtp = await smtpServer();
  const closed = await closedPort();
  const connection = connectionModule((config) => {
    Object.assign(config.endpoints.smtp, { default_port: closed, starttls_ports: [smtp.port], discovery_ports: [closed, smtp.port] });
  });
  const { ctx, saved, progress } = context({ input: {
    [names.address_env]: "Owner@Example.TEST",
    [names.password_env]: PASSWORD,
    [names.imap_host_env]: `127.0.0.1:${imap.port}`,
    [names.smtp_host_env]: "127.0.0.1",
    [names.allowed_senders_env]: "Boss@Example.com; @partner.test",
  } });
  const state = await connection.connect(ctx);
  assert.deepEqual(state, { state: "connected", account: { label: "Owner@example.test", detail: "Receives through 127.0.0.1 and sends through 127.0.0.1" } });
  assert.deepEqual(saved, [{
    [names.address_env]: "Owner@example.test",
    [names.password_env]: PASSWORD,
    [names.imap_host_env]: `127.0.0.1:${imap.port}`,
    [names.smtp_host_env]: `127.0.0.1:${smtp.port}`,
    [names.allowed_senders_env]: "Boss@example.com, @partner.test",
  }]);
  assert.equal(progress.length, 2);
  assert.equal(smtp.seen.plaintextAuth, false);

  const values = saved[0];
  assert.equal((await connection.probe(context({ values }).ctx)).state, "connected");
  const wrong = "wrong-app-password-4821";
  const expired = await connection.probe(context({ values: { ...values, [names.password_env]: wrong } }).ctx);
  assert.equal(expired.state, "expired");
  assert.match(expired.message, /did not accept the saved email address and app password/);
  assert.doesNotMatch(expired.message, new RegExp(wrong));
  assert.deepEqual(await connection.probe(context().ctx), { state: "not_connected" });

  const cleared = context({ values });
  assert.deepEqual(await connection.disconnect(cleared.ctx), { state: "not_connected" });
  assert.deepEqual(cleared.saved, [Object.fromEntries(Object.values(names).map((name) => [name, null]))]);
});

test("connect explains an app-password refusal and never signs in without encryption", async () => {
  const imap = await imapServer("NO [ALERT] Application-specific password required: https://support.google.com/accounts/answer/185833 (Failure)");
  const plaintext = await smtpServer(false);
  const connection = connectionModule((config) => {
    config.endpoints.smtp.starttls_ports = [plaintext.port];
  });
  const input = {
    [names.address_env]: "owner@example.test",
    [names.password_env]: "abcd efgh ijkl mnop",
    [names.imap_host_env]: `127.0.0.1:${imap.port}`,
    [names.smtp_host_env]: `127.0.0.1:${plaintext.port}`,
  };
  const refused = context({ input });
  const state = await connection.connect(refused.ctx);
  assert.equal(state.state, "error");
  assert.match(state.message, /needs an app password instead of the account password\. Use the password your provider issues for mail apps/);
  assert.deepEqual(refused.saved, []);

  const working = await imapServer();
  const unencrypted = context({ input: { ...input, [names.password_env]: PASSWORD, [names.imap_host_env]: `127.0.0.1:${working.port}` } });
  const blocked = await connection.connect(unencrypted.ctx);
  assert.equal(blocked.state, "error");
  assert.match(blocked.message, /does not offer STARTTLS on port \d+; Octopus never signs in without encryption/);
  assert.equal(plaintext.seen.plaintextAuth, false);
  assert.deepEqual(unencrypted.saved, []);

  const unknown = await connection.connect(context({ input: { [names.address_env]: "owner@example.test", [names.password_env]: PASSWORD } }).ctx);
  assert.deepEqual(unknown, { state: "error", message: "example.test has no built-in mail server settings; enter its IMAP and SMTP servers." });
});

test("the operation catalog declares one precise tool per mailbox action", () => {
  assertOperationCatalog(INTERFACE, "email", 8);
});
