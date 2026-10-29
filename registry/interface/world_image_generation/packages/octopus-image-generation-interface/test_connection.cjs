const assert = require("node:assert/strict");
const path = require("node:path");
const connection = require("./connection.cjs");
const descriptor = require(path.join(__dirname, "..", "..", "interface.json"));

function context(input, status) {
  const requests = [];
  const saved = [];
  const fetch = async (url, init) => {
    requests.push({ url: String(url), authorization: init.headers.Authorization });
    return { ok: status === 200, status };
  };
  return { requests, saved, ctx: { descriptor, values: {}, input, saveValues: async (map) => saved.push(map), fetch, progress() {} } };
}

(async () => {
  const accepted = context({ OCTOPUS_IMAGE_GENERATION_API_KEY: "sk-test" }, 200);
  assert.deepEqual(await connection.connect(accepted.ctx), { state: "connected", account: { label: "api.openai.com", detail: "API key accepted" } });
  assert.deepEqual(accepted.requests, [{ url: "https://api.openai.com/v1/models", authorization: "Bearer sk-test" }]);
  assert.deepEqual(accepted.saved, [{ OCTOPUS_IMAGE_GENERATION_API_KEY: "sk-test" }]);

  const rejected = context({ OCTOPUS_IMAGE_GENERATION_API_KEY: "sk-bad" }, 401);
  assert.equal((await connection.connect(rejected.ctx)).state, "expired");
  assert.equal(rejected.saved.length, 0);

  const custom = context({ OCTOPUS_IMAGE_GENERATION_API_KEY: "key", OCTOPUS_IMAGE_GENERATION_URL: "https://images.example/generate" }, 200);
  const unchecked = await connection.connect(custom.ctx);
  assert.equal(unchecked.state, "connected");
  assert.equal(custom.requests.length, 0);
  assert.match(unchecked.message, /first image request/);

  const plain = context({ OCTOPUS_IMAGE_GENERATION_API_KEY: "key", OCTOPUS_IMAGE_GENERATION_URL: "http://images.example/v1/images/generations" }, 200);
  assert.deepEqual(await connection.connect(plain.ctx), { state: "error", message: "The API URL must use https." });
  console.log("image generation connection: ok");
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
