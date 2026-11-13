const crypto = require("node:crypto");
const net = require("node:net");
const path = require("node:path");
const WebSocket = require("ws");

const REFUSED_CONNECTION_RETRY_MS = 2_000;
const REFUSED_CONNECTION_RETRY_STEP_MS = 50;

function handInArtifact(request) {
  const interfaceId = safeSegment(request?.interfaceId, "interfaceId");
  const octopusHome = absolutePath(request?.octopusHome, "octopusHome");
  const manifest = artifactManifest(request?.manifest);
  const endpoint = validateArtifactEndpoint(
    request?.endpoint,
    octopusHome,
    interfaceId,
  );
  const timeoutMs = positiveNumber(request?.timeoutMs, 120_000);
  const target = connectionTarget(endpoint);
  const frame = JSON.stringify(manifest);

  return new Promise((resolve, reject) => {
    let settled = false;
    let opened = false;
    let dispatched = false;
    let socket = null;
    let retry = null;
    const refusedUntil = Date.now() + REFUSED_CONNECTION_RETRY_MS;
    const finish = (error, verdict) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      clearTimeout(retry);
      try {
        if (socket?.readyState === WebSocket.OPEN) socket.close();
        else socket?.terminate();
      } catch {}
      if (error) {
        error.deliveryStage = dispatched ? "outcome-unknown" : "not-sent";
        reject(error);
      }
      else resolve(verdict);
    };
    const timer = setTimeout(
      () => finish(new Error(`artifact delivery verdict timed out after ${timeoutMs}ms`)),
      timeoutMs,
    );
    const connect = () => {
      let refused = false;
      const attempt = target.kind === "unix"
        ? new WebSocket("ws://localhost/", {
          createConnection: () => net.createConnection(target.path),
          handshakeTimeout: timeoutMs,
          maxPayload: 1024 * 1024,
          perMessageDeflate: false,
        })
        : new WebSocket(target.url, {
          handshakeTimeout: timeoutMs,
          maxPayload: 1024 * 1024,
          perMessageDeflate: false,
        });
      socket = attempt;
      attempt.once("open", () => {
        if (settled) return;
        opened = true;
        dispatched = true;
        try {
          attempt.send(frame, (error) => {
            if (error) finish(error);
          });
        } catch (error) { finish(error); }
      });
      attempt.once("message", (data, isBinary) => {
        if (isBinary) {
          finish(new Error("artifact delivery returned a binary verdict frame"));
          return;
        }
        let verdict;
        try {
          verdict = JSON.parse(Buffer.from(data).toString("utf8"));
        } catch (error) {
          finish(new Error(`artifact delivery returned malformed verdict JSON: ${error.message}`));
          return;
        }
        if (!isObject(verdict)) {
          finish(new Error("artifact delivery verdict must be a JSON object"));
          return;
        }
        finish(null, verdict);
      });
      attempt.once("error", (error) => {
        if (!settled && !opened && target.kind === "unix" && error?.code === "ECONNREFUSED" && Date.now() < refusedUntil) {
          refused = true;
          retry = setTimeout(connect, REFUSED_CONNECTION_RETRY_STEP_MS);
          return;
        }
        finish(error);
      });
      attempt.once("close", (code, reason) => {
        if (settled || refused) return;
        const detail = Buffer.from(reason).toString("utf8");
        finish(new Error(
          `${opened ? "artifact delivery closed before a verdict" : "artifact delivery could not connect"} (${code}${detail ? `: ${detail}` : ""})`,
        ));
      });
    };
    connect();
  });
}

async function captureArtifactDelivery(operation) {
  let stage = "not-sent";
  let attempted = false;
  try {
    return await operation(async (request) => {
      if (attempted) throw new Error("artifact delivery capture already has an attempt");
      attempted = true;
      const delivery = handInArtifact(request);
      stage = "outcome-unknown";
      let verdict;
      try { verdict = await delivery; }
      catch (error) {
        if (error?.deliveryStage === "not-sent") stage = "not-sent";
        throw error;
      }
      if (isObject(verdict?.admitted) && Number.isFinite(verdict.admitted.seq)) stage = "admitted";
      else if (isObject(verdict?.refused)) stage = "refused";
      return verdict;
    });
  } catch (error) {
    return { deliveryError: { stage, message: error instanceof Error ? error.message : String(error) } };
  }
}

function defaultInterfaceEndpoint(octopusHome, interfaceId) {
  return `unix://${path.join(
    absolutePath(octopusHome, "octopusHome"),
    "app-server-control",
    `${crypto.createHash("sha256").update(safeSegment(interfaceId, "interfaceId")).digest("hex").slice(0, 24)}.sock`,
  )}`;
}

function normalizedEndpoint(value, octopusHome, interfaceId) {
  const configured = stringValue(value);
  if (!configured || configured === "unix://") {
    return defaultInterfaceEndpoint(octopusHome, interfaceId);
  }
  if (configured.startsWith("/")) return `unix://${configured}`;
  if (configured.startsWith("http://")) return `ws://${configured.slice("http://".length)}`;
  if (configured.startsWith("https://")) return `wss://${configured.slice("https://".length)}`;
  return configured;
}

function connectionTarget(endpoint) {
  if (endpoint.startsWith("unix://")) {
    return { kind: "unix", path: absolutePath(endpoint.slice("unix://".length), "unix endpoint") };
  }
  const url = new URL(endpoint);
  if (url.protocol !== "ws:" && url.protocol !== "wss:") {
    throw new Error(`unsupported artifact endpoint protocol: ${url.protocol}`);
  }
  return { kind: "websocket", url: url.toString() };
}

function validateArtifactEndpoint(value, octopusHome, interfaceId) {
  const endpoint = normalizedEndpoint(value, octopusHome, interfaceId);
  connectionTarget(endpoint);
  return endpoint;
}

function artifactManifest(value) {
  if (!isObject(value)) throw new Error("artifact manifest must be an object");
  const manifest = {
    label: requiredString(value.label, "artifact.label"),
    payload_path: absolutePath(value.payload_path, "artifact.payload_path"),
  };
  const payloadHash = stringValue(value.payload_content_hash);
  if (payloadHash) manifest.payload_content_hash = payloadHash;
  return manifest;
}

function safeSegment(value, field) {
  const segment = requiredString(value, field);
  if (segment.length > 96 || !/^[A-Za-z0-9_-]+$/.test(segment)) {
    throw new Error(`${field} must contain only letters, digits, underscore, or hyphen`);
  }
  return segment;
}

function absolutePath(value, field) {
  const input = requiredString(value, field);
  if (!path.isAbsolute(input)) throw new Error(`${field} must be absolute`);
  return path.normalize(input);
}

function isObject(value) {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function stringValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function requiredString(value, field) {
  const output = stringValue(value);
  if (!output) throw new Error(`${field} is required`);
  return output;
}

function positiveNumber(value, fallback) {
  return typeof value === "number" && Number.isFinite(value) && value > 0
    ? value
    : fallback;
}

module.exports = {
  captureArtifactDelivery,
  defaultInterfaceEndpoint,
  handInArtifact,
  normalizedEndpoint,
  validateArtifactEndpoint,
};
