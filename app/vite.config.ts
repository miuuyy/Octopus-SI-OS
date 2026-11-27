import { createRequire } from "node:module";
import fsSync from "node:fs";
import os from "node:os";
import path from "node:path";
import { defineConfig, type Plugin, type ViteDevServer } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import type { IncomingMessage, ServerResponse } from "node:http";

const require = createRequire(import.meta.url);
const COOKIE_NAME = "octopus_client_access";
const APP_DIR = path.dirname(new URL(import.meta.url).pathname);

function octopusClientServer(): Plugin {
  return {
    name: "octopus-client-server",
    configureServer(server: ViteDevServer) {
      const appDir = APP_DIR;
      assertDevServerPrerequisites(appDir, require);
      const { createClientAccessController } = require("./electron/clientAccess.cjs");
      const { createClientProjectionStore } = require("./electron/clientDataSource.cjs");
      const { projectionReadRequest } = require("./electron/projectionReadContract.cjs");
      const { createClientSurfaceDocumentStore, filterClientSurfaceSnapshot } = require("./electron/surfaceDocuments.cjs");
      const repoRoot = path.resolve(appDir, "..");
      const octopusHome = (process.env.OCTOPUS_HOME ?? "").trim() || path.join(os.homedir(), ".octopus");
      const installationRoot = (() => {
        const configured = (process.env.OCTOPUS_INSTALLATION_ROOT ?? "").trim();
        if (!configured) return repoRoot;
        if (!path.isAbsolute(configured)) throw new Error("OCTOPUS_INSTALLATION_ROOT must be absolute");
        return configured;
      })();
      const localPackageRegistryRoot = configuredAbsolutePath(
        process.env.OCTOPUS_LOCAL_PACKAGE_REGISTRY,
        path.join(repoRoot, "registry"),
        "OCTOPUS_LOCAL_PACKAGE_REGISTRY",
      );
      const localPackageLibraryRoot = configuredAbsolutePath(
        process.env.OCTOPUS_LOCAL_PACKAGE_LIBRARY,
        path.join(repoRoot, "app", "distribution", "local-packages"),
        "OCTOPUS_LOCAL_PACKAGE_LIBRARY",
      );
      const {
        assertOperatorChatArtifactBatch,
        createOperatorChatClientLoader,
        isOperatorChatClientInterfaceCandidate,
      } = require("./electron/operatorChatClientContract.cjs");
      const operatorChatClientLoader = createOperatorChatClientLoader({
        packageRoots: () => ({
          sourceRegistryRoot: localPackageRegistryRoot,
          activeRegistryRoot: path.join(octopusHome, "registry"),
          libraryRoot: localPackageLibraryRoot,
          stateRoot: path.join(octopusHome, "client", "package-state"),
        }),
        clientPath: () => path.join(
          octopusHome,
          "registry",
          "interface",
          "world_operator_chat",
          "client.cjs",
        ),
      });
      const access = createClientAccessController();
      const stores = new Map<"live" | "lab", ReturnType<typeof createClientProjectionStore>>();
      const projectionClients = new Map<"live" | "lab", Set<ServerResponse>>([
        ["live", new Set()],
        ["lab", new Set()],
      ]);
      const surfaceClients = new Set<ServerResponse>();
      let surfaceStore: ReturnType<typeof createClientSurfaceDocumentStore> | null = null;
      let artifactStreamStore: any = null;
      const clientArtifactsRoot = (interfaceId: string) => path.join(
        octopusHome,
        "interface-hand-in",
        interfaceId,
        "client",
      );
      const authorizeClientInterfaceArtifactSubmission = async (request: Record<string, unknown>) => {
        if (!isOperatorChatClientInterfaceCandidate(request?.interfaceId)) return;
        const { assertGenericOperatorChatArtifactAllowed } = await operatorChatClientLoader.load();
        await assertGenericOperatorChatArtifactAllowed(request);
      };

      const getStore = (profile: "live" | "lab") => {
        let store = stores.get(profile);
        if (store) return store;
        store = createClientProjectionStore(profile, {
          resolveRepoRoot: () => fsSync.existsSync(path.join(repoRoot, "registry", "interface")) ? repoRoot : "",
          octopusHome: () => octopusHome,
          onChange: (generation: number, kind: string) => {
            for (const client of projectionClients.get(profile) ?? []) {
              client.write(`data: ${JSON.stringify({
                generation,
                kind: ["ledger", "runs", "files"].includes(kind) ? kind : "structure",
              })}\n\n`);
            }
          },
        });
        stores.set(profile, store);
        return store;
      };

      const labFixturePath = path.resolve(appDir, "electron", "fixtureProjection.cjs");
      delete require.cache[require.resolve(labFixturePath)];
      server.watcher.add(labFixturePath);
      server.watcher.on("change", (file) => {
        if (path.resolve(file) !== labFixturePath) return;
        delete require.cache[require.resolve(labFixturePath)];
        stores.delete("lab");
        for (const client of projectionClients.get("lab") ?? []) {
          client.write(`data: ${JSON.stringify({ generation: Date.now() })}\n\n`);
        }
      });

      const getSurfaceStore = () => {
        if (surfaceStore) return surfaceStore;
        surfaceStore = createClientSurfaceDocumentStore({
          root: path.join(appDir, "client-surfaces"),
          onChange: (generation: number) => {
            for (const client of surfaceClients) client.write(`data: ${JSON.stringify({ generation })}\n\n`);
          },
        });
        return surfaceStore;
      };

      const getArtifactStreamStore = () => {
        if (artifactStreamStore) return artifactStreamStore;
        const { handInArtifact } = require("./electron/interfaceTransport.cjs");
        const { createClientArtifactStreamStore } = require("./electron/clientArtifacts.cjs");
        artifactStreamStore = createClientArtifactStreamStore({
          clientArtifactsRoot,
          projectionStore: () => getStore("live"),
          handInArtifact: (input: Record<string, unknown>) => handInArtifact({ ...input, octopusHome }),
          authorizeSubmission: authorizeClientInterfaceArtifactSubmission,
        });
        return artifactStreamStore;
      };

      server.httpServer?.once("close", () => {
        surfaceStore?.close();
        void artifactStreamStore?.abortAll();
      });

      server.middlewares.use("/__octopus/access", (req, res) => {
        void handleAccess(req, res, access);
      });

      server.middlewares.use("/__octopus/projection", (req, res) => {
        void withAccess(req, res, access, ["live", "lab"], async (session) => {
          if (req.method !== "POST") return sendJson(res, 405, { error: "projection reads require POST" });
          const request = await readJson(req);
          sendJson(res, 200, await getStore(session.profile).read(request));
        });
      });

      server.middlewares.use("/__octopus/providers", (req, res) => {
        void withAccess(req, res, access, ["live", "lab"], async (session) => {
          const registryRoot = session.profile === "live" ? path.join(octopusHome, "registry") : "";
          const { providers } = await getStore(session.profile).read(projectionReadRequest("projection.operator.providers.list"));
          sendJson(res, 200, { repoRoot: session.profile === "live" ? repoRoot : null, registryRoot: registryRoot || null, providers });
        });
      });

      server.middlewares.use("/__octopus/operator-chat-artifacts", (req, res) => {
        void withAccess(req, res, access, ["live"], async () => {
          if (req.method !== "POST") {
            return sendJson(res, 405, { error: "operator chat artifact reads require POST" });
          }
          const request = await readJson(req);
          const { readOperatorChatArtifacts } = await operatorChatClientLoader.load();
          const { readContentAddressedArtifactPayload } = require("./electron/contentAddressedArtifact.cjs");
          const batch = await readOperatorChatArtifacts({
            octopusHome: () => octopusHome,
            readArtifactPayload: (input: Record<string, unknown>) => readContentAddressedArtifactPayload({
              ...input,
              blobsRoot: path.join(octopusHome, "ledger", "blobs"),
            }),
          }, request);
          sendJson(res, 200, assertOperatorChatArtifactBatch(batch));
        });
      });

      server.middlewares.use("/__octopus/operator-turn-control", (req, res) => {
        void withAccess(req, res, access, ["live"], async () => {
          if (req.method !== "POST") {
            return sendJson(res, 405, { error: "operator turn control requires POST" });
          }
          const request = await readJson(req);
          const { submitOperatorTurnControl } = await operatorChatClientLoader.load();
          const { captureArtifactDelivery } = require("./electron/interfaceTransport.cjs");
          sendJson(res, 200, await captureArtifactDelivery((deliver: (input: Record<string, unknown>) => Promise<unknown>) => submitOperatorTurnControl({
            octopusHome: () => octopusHome,
            clientArtifactsRoot,
            handInArtifact: (input: Record<string, unknown>) => deliver({ ...input, octopusHome }),
          }, request)));
        });
      });

      server.middlewares.use("/__octopus/validate-endpoint", (req, res) => {
        void withAccess(req, res, access, ["live"], async () => {
          if (req.method !== "POST") return sendJson(res, 405, { valid: false, endpoint: "", error: "endpoint validation requires POST" });
          const request = await readJson(req);
          const endpoint = typeof request?.endpoint === "string" ? request.endpoint : "";
          const { validateArtifactEndpoint } = require("./electron/interfaceTransport.cjs");
          const normalized = validateArtifactEndpoint(endpoint, octopusHome, "world_operator_chat");
          sendJson(res, 200, { valid: true, endpoint: normalized });
        });
      });

      server.middlewares.use("/__octopus/submit", (req, res) => {
        void withAccess(req, res, access, ["live", "lab"], async (session) => {
          if (req.method !== "POST") return sendJson(res, 405, { error: "submit requires POST" });
          const request = await readJson(req);
          if (session.profile === "lab") {
            return sendJson(res, 200, await getStore("lab").submitChatTurn(request));
          }
          try {
            const { handInArtifact, normalizedEndpoint } = require("./electron/interfaceTransport.cjs");
            const { readContentAddressedArtifactPayload } = require("./electron/contentAddressedArtifact.cjs");
            const { submitOperatorTurn } = await operatorChatClientLoader.load();
            const defaultChatEndpoint = normalizedEndpoint(process.env.OCTOPUS_APP_LISTEN, octopusHome, "world_operator_chat");
            const outcome = await submitOperatorTurn({
              clientArtifactsRoot,
              octopusHome: () => octopusHome,
              installationRoot: () => installationRoot,
              registryRoot: () => path.join(octopusHome, "registry"),
              handInArtifact: (input: Record<string, unknown>) => handInArtifact({
                ...input,
                endpoint: typeof input.endpoint === "string" && input.endpoint.trim() !== "unix://" ? input.endpoint : defaultChatEndpoint,
                octopusHome,
              }),
              readArtifactPayload: (input: Record<string, unknown>) => readContentAddressedArtifactPayload({
                ...input,
                blobsRoot: path.join(octopusHome, "ledger", "blobs"),
              }),
              readProjection: (method: string) => getStore("live").read(projectionReadRequest(method)),
            }, {
              ...request,
              attachments: (Array.isArray(request?.attachments) ? request.attachments : []).map((input: { name?: string; label?: string; type?: string; base64?: string }) => ({
                name: input?.name,
                label: input?.label,
                type: input?.type,
                bytes: bufferFromBase64(input?.base64, "attachment.base64"),
              })),
            });
            sendJson(res, 200, outcome);
          } catch (error) {
            const { OPERATOR_CHAT_SUBMIT_ERROR_MESSAGE } = require("./electron/operatorChatSubmitResult.cjs");
            console.error(error);
            sendJson(res, 503, { error: OPERATOR_CHAT_SUBMIT_ERROR_MESSAGE });
          }
        });
      });

      server.middlewares.use("/__octopus/chat-steer", (req, res) => {
        void withAccess(req, res, access, ["live", "lab"], async (session) => {
          if (req.method !== "POST") return sendJson(res, 405, { error: "chat steer requires POST" });
          const request = await readJson(req);
          if (session.profile === "lab") {
            return sendJson(res, 200, await getStore("lab").submitChatSteer(request));
          }
          if (Array.isArray(request?.attachments) && request.attachments.length > 0) {
            throw new Error("Attachment submission requires the desktop attachment staging host.");
          }
          const { handInArtifact, normalizedEndpoint } = require("./electron/interfaceTransport.cjs");
          const { readContentAddressedArtifactPayload } = require("./electron/contentAddressedArtifact.cjs");
          const { submitOperatorTurn } = await operatorChatClientLoader.load();
          const defaultChatEndpoint = normalizedEndpoint(process.env.OCTOPUS_APP_LISTEN, octopusHome, "world_operator_chat");
          const outcome = await submitOperatorTurn({
            clientArtifactsRoot,
            octopusHome: () => octopusHome,
            installationRoot: () => installationRoot,
            registryRoot: () => path.join(octopusHome, "registry"),
            handInArtifact: (input: Record<string, unknown>) => handInArtifact({
              ...input,
              endpoint: typeof input.endpoint === "string" && input.endpoint.trim() !== "unix://" ? input.endpoint : defaultChatEndpoint,
              octopusHome,
            }),
            readArtifactPayload: (input: Record<string, unknown>) => readContentAddressedArtifactPayload({
              ...input,
              blobsRoot: path.join(octopusHome, "ledger", "blobs"),
            }),
            readProjection: (method: string) => getStore("live").read(projectionReadRequest(method)),
          }, { ...request, attachments: [] });
          sendJson(res, 200, {
            interfaceId: outcome.chatInterfaceId,
            runId: outcome.runId,
            label: outcome.label,
            messageId: outcome.messageId,
            attachmentCount: outcome.attachmentCount,
            contentHash: outcome.contentHash,
            messageRecovered: outcome.messageRecovered,
            verdict: outcome.messageVerdict,
          });
        });
      });

      server.middlewares.use("/__octopus/chat-pins", (req, res) => {
        void withAccess(req, res, access, ["live", "lab"], async (session) => {
          if (req.method !== "POST") return sendJson(res, 405, { error: "chat pin updates require POST" });
          const request = await readJson(req);
          if (session.profile === "lab") {
            return sendJson(res, 200, await getStore("lab").updateChatPins(request));
          }
          const { updateOperatorChatPins } = await operatorChatClientLoader.load();
          const outcome = await updateOperatorChatPins({
            octopusHome: () => octopusHome,
            installationRoot: () => installationRoot,
          }, request);
          sendJson(res, 200, outcome);
        });
      });

      server.middlewares.use("/__octopus/interface-artifact", (req, res) => {
        void withAccess(req, res, access, ["live"], async () => {
          if (req.method !== "POST") return sendJson(res, 405, { error: "interface artifact submit requires POST" });
          const request = await readJson(req);
          const { handInArtifact } = require("./electron/interfaceTransport.cjs");
          const { submitClientInterfaceArtifact } = require("./electron/clientArtifacts.cjs");
          const outcome = await submitClientInterfaceArtifact({
            clientArtifactsRoot,
            projectionStore: () => getStore("live"),
            handInArtifact: (input: Record<string, unknown>) => handInArtifact({ ...input, octopusHome }),
            authorizeSubmission: authorizeClientInterfaceArtifactSubmission,
          }, request);
          sendJson(res, 200, outcome);
        });
      });

      server.middlewares.use("/__octopus/artifact-stream", (req, res, next) => {
        const operation = String(req.url ?? "").split("?")[0].replace(/^\/+/, "");
        if (operation !== "begin" && operation !== "append" && operation !== "finalize" && operation !== "abort") return next();
        void withAccess(req, res, access, ["live"], async () => {
          if (req.method !== "POST") return sendJson(res, 405, { error: `artifact stream ${operation} requires POST` });
          const request = await readJson(req);
          const owner = readCookie(req, COOKIE_NAME);
          const streamStore = getArtifactStreamStore();
          const outcome = operation === "begin"
            ? await streamStore.begin(owner, request)
            : operation === "append"
              ? await streamStore.append(owner, request)
              : operation === "finalize"
                ? await streamStore.finalize(owner, request)
                : await streamStore.abort(owner, request);
          sendJson(res, 200, outcome);
        });
      });

      server.middlewares.use("/__octopus/client-surfaces", (req, res) => {
        void withAccess(req, res, access, ["live", "lab"], async (session) => {
          if (req.method !== "GET") return sendJson(res, 405, { error: "client surface documents require GET" });
          sendJson(res, 200, filterClientSurfaceSnapshot(await getSurfaceStore().snapshot(), session.profile));
        });
      });

      server.middlewares.use("/__octopus/client-surface-events", (req, res) => {
        const session = requireHttpAccess(req, res, access, ["lab"]);
        if (!session) return;
        const store = getSurfaceStore();
        openEventStream(res, store.generation());
        surfaceClients.add(res);
        store.ensureWatcher();
        req.on("close", () => surfaceClients.delete(res));
      });

      server.middlewares.use("/__octopus/events", (req, res) => {
        const session = requireHttpAccess(req, res, access, ["live", "lab"]);
        if (!session) return;
        const store = getStore(session.profile);
        openEventStream(res, store.generation());
        projectionClients.get(session.profile)?.add(res);
        store.ensureWatchers();
        req.on("close", () => projectionClients.get(session.profile)?.delete(res));
      });
    },
  };
}

async function handleAccess(req: IncomingMessage, res: ServerResponse, access: any) {
  try {
    const operation = String(req.url ?? "").split("?")[0].replace(/^\/+/, "");
    const token = readCookie(req, COOKIE_NAME);
    if (operation === "status" && req.method === "GET") return sendJson(res, 200, access.status(token));
    if (operation === "configure" && req.method === "POST") {
      const session = access.configure((await readJson(req)).password);
      setAccessCookie(res, session.token, session.expiresAt);
      return sendJson(res, 200, access.status(session.token));
    }
    if (operation === "unlock" && req.method === "POST") {
      const session = access.unlock((await readJson(req)).password);
      setAccessCookie(res, session.token, session.expiresAt);
      return sendJson(res, 200, access.status(session.token));
    }
    if (operation === "lock" && req.method === "POST") {
      const status = access.lock(token);
      clearAccessCookie(res);
      return sendJson(res, 200, status);
    }
    if (operation === "lab-status" && req.method === "GET") {
      return sendJson(res, 200, access.labAccess(token));
    }
    if (operation === "lab-update" && req.method === "POST") {
      return sendJson(res, 200, access.updateLabAccess(token, (await readJson(req)).password));
    }
    if (operation === "details" && req.method === "GET") {
      return sendJson(res, 200, access.accessDetails(token));
    }
    if (operation === "administrator-update" && req.method === "POST") {
      const request = await readJson(req);
      return sendJson(res, 200, access.updateAdministratorAccess(token, request.currentPassword, request.password));
    }
    sendJson(res, 404, { error: "Unknown client access operation" });
  } catch (error) {
    sendJson(res, 401, { error: formatError(error) });
  }
}

async function withAccess(
  req: IncomingMessage,
  res: ServerResponse,
  access: any,
  profiles: Array<"live" | "lab">,
  handler: (session: { profile: "live" | "lab" }) => Promise<void>,
) {
  const session = requireHttpAccess(req, res, access, profiles);
  if (!session) return;
  try {
    await handler(session);
  } catch (error) {
    sendJson(res, 400, { error: formatError(error) });
  }
}

function requireHttpAccess(req: IncomingMessage, res: ServerResponse, access: any, profiles: Array<"live" | "lab">) {
  try {
    return access.requireSession(readCookie(req, COOKIE_NAME), profiles);
  } catch (error) {
    sendJson(res, 401, { error: formatError(error) });
    return null;
  }
}

function readJson(req: IncomingMessage): Promise<any> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    let bytes = 0;
    req.on("data", (chunk: Buffer) => {
      bytes += chunk.byteLength;
      if (bytes > 8 * 1024 * 1024) {
        reject(new Error("Request body exceeds 8 MiB"));
        req.destroy();
        return;
      }
      chunks.push(chunk);
    });
    req.on("end", () => {
      try {
        resolve(JSON.parse(Buffer.concat(chunks).toString("utf8") || "{}"));
      } catch (error) {
        reject(error);
      }
    });
    req.on("error", reject);
  });
}

function sendJson(res: ServerResponse, status: number, value: unknown) {
  if (res.headersSent) return;
  res.statusCode = status;
  res.setHeader("content-type", "application/json");
  res.setHeader("cache-control", "no-store");
  res.end(JSON.stringify(value));
}

function readCookie(req: IncomingMessage, name: string) {
  const prefix = `${name}=`;
  for (const part of String(req.headers.cookie ?? "").split(";")) {
    const value = part.trim();
    if (value.startsWith(prefix)) return decodeURIComponent(value.slice(prefix.length));
  }
  return "";
}

function setAccessCookie(res: ServerResponse, token: string, expiresAt: string) {
  res.setHeader("set-cookie", `${COOKIE_NAME}=${encodeURIComponent(token)}; Path=/; HttpOnly; SameSite=Strict; Expires=${new Date(expiresAt).toUTCString()}`);
}

function clearAccessCookie(res: ServerResponse) {
  res.setHeader("set-cookie", `${COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0`);
}

function openEventStream(res: ServerResponse, generation: number) {
  res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache", connection: "keep-alive" });
  res.write(`data: ${JSON.stringify({ generation })}\n\n`);
}

function bufferFromBase64(value: unknown, field: string): Buffer {
  const compact = typeof value === "string" ? value.replace(/[\r\n\t ]+/g, "") : "";
  if (!compact || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(compact)) {
    throw new Error(`${field} must contain canonical base64 bytes`);
  }
  const bytes = Buffer.from(compact, "base64");
  if (bytes.toString("base64") !== compact) throw new Error(`${field} must contain canonical base64 bytes`);
  return bytes;
}

function configuredAbsolutePath(value: string | undefined, fallback: string, field: string) {
  const configured = (value ?? "").trim();
  if (!configured) return fallback;
  if (!path.isAbsolute(configured)) throw new Error(`${field} must be absolute`);
  return configured;
}

function assertDevServerPrerequisites(appDir: string, loadModule: NodeRequire) {
  let sqlite: { DatabaseSync?: unknown } | null = null;
  try {
    sqlite = loadModule("node:sqlite");
  } catch (error) {
    throw new Error(
      `Octopus client dev server requires Node.js with built-in node:sqlite (Node 24 LTS; current: ${process.versions.node}): ${formatError(error)}`,
    );
  }
  if (typeof sqlite?.DatabaseSync !== "function") {
    throw new Error(
      `Octopus client dev server requires Node.js with node:sqlite DatabaseSync (Node 24 LTS; current: ${process.versions.node}).`,
    );
  }
  const manifest = JSON.parse(fsSync.readFileSync(path.join(appDir, "package.json"), "utf8")) as {
    dependencies?: Record<string, string>;
  };
  const missingPackages = Object.keys(manifest.dependencies ?? {}).filter(
    (name) => !fsSync.existsSync(path.join(appDir, "node_modules", name, "package.json")),
  );
  if (missingPackages.length > 0) {
    throw new Error(
      `Octopus client dev server cannot start with missing packages (${missingPackages.join(", ")}); run \`npm ci --prefix app\` first.`,
    );
  }
}

function formatError(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

export default defineConfig({
  base: "./",
  plugins: [tailwindcss(), react(), octopusClientServer(), {
    name: "support-development-csp",
    transformIndexHtml(html, context) {
      return context.server && context.filename === path.resolve(APP_DIR, "support.html")
        ? html.replace("script-src 'self';", "script-src 'self' 'unsafe-inline';")
        : html;
    },
  }],
  worker: { format: "es" },
  build: { rollupOptions: { input: { main: path.resolve(APP_DIR, "index.html"), support: path.resolve(APP_DIR, "support.html") } } },
  server: { host: "127.0.0.1", port: 5174, fs: { strict: true, allow: [APP_DIR] } },
  preview: { host: "127.0.0.1", port: 4174 },
});
