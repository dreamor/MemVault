// Offline contract tests: fetch and the host client are mocked; no backend,
// extractor, model, or memory store is used.
import test from "node:test";
import assert from "node:assert/strict";

let moduleCounter = 0;
const envKeys = [
  "MEMVAULT_HTTP_URL",
  "MEMVAULT_HOOK_EXTRACT",
  "MEMVAULT_AGENT_ID",
  "MEMVAULT_API_KEY",
];

function fakeBackend(t, handler, env = {}) {
  const originalFetch = globalThis.fetch;
  const originalEnv = new Map(envKeys.map((key) => [key, process.env[key]]));
  t.after(() => {
    globalThis.fetch = originalFetch;
    for (const [key, value] of originalEnv) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
  });
  for (const key of envKeys) delete process.env[key];
  Object.assign(process.env, {
    MEMVAULT_HTTP_URL: "http://memvault.test",
    MEMVAULT_HOOK_EXTRACT: "1",
    ...env,
  });

  const requests = [];
  globalThis.fetch = async (url, init) => {
    assert.equal(new URL(url).origin, "http://memvault.test");
    const request = {
      url: new URL(url).pathname + new URL(url).search,
      method: init.method,
      body: JSON.parse(init.body),
      headers: init.headers,
    };
    const payload = handler(request);
    const response = payload instanceof Response
      ? payload
      : new Response(JSON.stringify(payload), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
    requests.push({ ...request, status: response.status });
    return response;
  };
  return requests;
}

// Actor config is captured at module load; reload it like the host would.
async function loadPlugin() {
  moduleCounter += 1;
  return import(`./memvault.mjs?test=${moduleCounter}`);
}

function jsonResponse(data) {
  return { ok: true, data };
}

async function runIdle(plugin) {
  await plugin.event({
    event: { type: "session.idle", properties: { sessionID: "session-1" } },
  });
}

function fakeClient(text) {
  return {
    session: {
      messages: async () => ({ data: [{ parts: [{ type: "text", text }] }] }),
    },
  };
}

function captureWarnings(t) {
  const warnings = [];
  const originalWarn = console.warn;
  console.warn = (...args) => warnings.push(args.join(" "));
  t.after(() => { console.warn = originalWarn; });
  return warnings;
}

const namedAgent = "opencode-contract-agent";
const fakeKey = "fake-opencode-api-key";

function keyedResponse(request) {
  // Operational routes authenticate from headers, not JSON agent_id.
  const actor = request.headers["x-memvault-agent-id"] ?? "admin";
  if (actor !== namedAgent || request.headers["x-memvault-api-key"] !== fakeKey) {
    return new Response("unauthorized", { status: 401 });
  }
  if (request.url === "/api/session?output=plain") {
    assert.equal(request.body.agent_id, actor);
    return new Response("remember this", { status: 200 });
  }
  assert.equal(request.url, "/api/extract");
  assert.equal(request.body.agent_id, actor);
  assert.equal(request.body.source_id, actor);
  return jsonResponse({ memories: [{ content: "fake draft" }], saved_ids: ["fake-draft-1"], warning: null });
}

test("session.idle extraction asks the server to persist the draft", async (t) => {
  const requests = fakeBackend(t, () => jsonResponse({
    memories: [{ content: "I always prefer dark mode" }],
    saved_ids: ["fake-draft-1"],
    warning: null,
  }));
  const { MemVault } = await loadPlugin();
  const plugin = await MemVault({ client: fakeClient("I always prefer dark mode") });
  await runIdle(plugin);

  const extract = requests.find((r) => r.url === "/api/extract");
  assert.ok(extract, "idle event must call /api/extract");
  assert.equal(extract.body.auto_save, true, "auto_save must opt into persistence");
  assert.equal(extract.body.source_id, "opencode", "provenance uses source_id");
  assert.equal(extract.body.agent_id, "opencode");
  assert.match(extract.body.text, /dark mode/);
});

test("repeated idles extract only messages newer than the last run (C10)", async (t) => {
  const requests = fakeBackend(t, () => jsonResponse({ memories: [], saved_ids: [], warning: null }));
  const all = [{ parts: [{ type: "text", text: "first turn text" }] }];
  const client = { session: { messages: async () => ({ data: all }) } };
  const { MemVault } = await loadPlugin();
  const plugin = await MemVault({ client });

  await runIdle(plugin);
  await runIdle(plugin);
  let extracts = requests.filter((r) => r.url === "/api/extract");
  assert.equal(extracts.length, 1, "an idle with no new messages must not re-send the transcript");

  all.push({ parts: [{ type: "text", text: "second turn text" }] });
  await runIdle(plugin);
  extracts = requests.filter((r) => r.url === "/api/extract");
  assert.equal(extracts.length, 2);
  assert.match(extracts[1].body.text, /second turn text/);
  assert.doesNotMatch(extracts[1].body.text, /first turn text/, "already-extracted turns must not be re-sent");
});

test("a failed extraction is retried at the next idle (C10)", async (t) => {
  let fail = true;
  const requests = fakeBackend(t, () => fail
    ? { ok: false, error: "backend down" }
    : jsonResponse({ memories: [], saved_ids: [], warning: null }));
  const warnings = captureWarnings(t);
  const { MemVault } = await loadPlugin();
  const plugin = await MemVault({ client: fakeClient("remember this") });
  await runIdle(plugin);
  fail = false;
  await runIdle(plugin);
  const extracts = requests.filter((r) => r.url === "/api/extract");
  assert.equal(extracts.length, 2, "the failed tail must be retried");
  assert.match(extracts[1].body.text, /remember this/);
  assert.ok(warnings.some((w) => w.includes("request failed")));
});

test("named agent headers authorize recall and extraction with a lazily provisioned key", async (t) => {
  const requests = fakeBackend(t, keyedResponse, { MEMVAULT_AGENT_ID: namedAgent });
  const { MemVault } = await loadPlugin();
  const plugin = await MemVault({ client: fakeClient("remember this") });
  process.env.MEMVAULT_API_KEY = fakeKey;

  const output = { system: [] };
  await plugin["experimental.chat.system.transform"]({}, output);
  await runIdle(plugin);
  assert.deepEqual(output.system, ["remember this"]);
  assert.deepEqual(requests.map((r) => r.url), ["/api/session?output=plain", "/api/extract"]);
  for (const request of requests) {
    assert.equal(request.status, 200);
    assert.equal(request.headers["x-memvault-agent-id"], namedAgent);
    assert.equal(request.headers["x-memvault-api-key"], fakeKey);
    assert.equal(request.body.agent_id, namedAgent);
  }
});

test("default actor is opencode on recall and extraction without a key", async (t) => {
  const requests = fakeBackend(t, (r) => r.url === "/api/extract"
    ? jsonResponse({ memories: [], saved_ids: [], warning: null })
    : new Response("remember this"));
  const { MemVault } = await loadPlugin();
  const plugin = await MemVault({ client: fakeClient("remember this") });
  await plugin["experimental.chat.system.transform"]({}, { system: [] });
  await runIdle(plugin);
  assert.equal(requests.length, 2);
  for (const request of requests) {
    assert.equal(request.headers["x-memvault-agent-id"], "opencode");
    assert.equal(request.body.agent_id, "opencode");
    assert.equal(request.headers["x-memvault-api-key"], undefined);
  }
});

test("401 for a wrong named-agent key is visible and does not advance the extraction watermark", async (t) => {
  const requests = fakeBackend(t, keyedResponse, {
    MEMVAULT_AGENT_ID: namedAgent,
    MEMVAULT_API_KEY: "fake-wrong-api-key",
  });
  const warnings = captureWarnings(t);
  const { MemVault } = await loadPlugin();
  const plugin = await MemVault({ client: fakeClient("remember this") });
  const output = { system: [] };
  await plugin["experimental.chat.system.transform"]({}, output);
  await runIdle(plugin);
  await runIdle(plugin);
  assert.deepEqual(output.system, [], "unauthorized recall must not inject context");
  assert.deepEqual(requests.map((r) => r.status), [401, 401, 401]);
  assert.ok(warnings.some((w) => w.includes("401")));
  assert.ok(warnings.some((w) => w.includes("persisted 0") && w.includes("request failed")));

  process.env.MEMVAULT_API_KEY = fakeKey;
  await plugin["experimental.chat.system.transform"]({}, output);
  await runIdle(plugin);
  await runIdle(plugin);
  assert.deepEqual(output.system, ["remember this"]);
  const extracts = requests.filter((r) => r.url === "/api/extract");
  assert.deepEqual(extracts.map((r) => r.status), [401, 401, 200]);
  assert.match(extracts[2].body.text, /remember this/, "failed extraction remains eligible for retry");
});

test("a valid key for another actor is rejected", async (t) => {
  const requests = fakeBackend(t, keyedResponse, {
    MEMVAULT_AGENT_ID: "opencode-other-contract-agent",
    MEMVAULT_API_KEY: fakeKey,
  });
  captureWarnings(t);
  const { MemVault } = await loadPlugin();
  const plugin = await MemVault({ client: fakeClient("remember this") });
  await runIdle(plugin);
  assert.equal(requests[0].status, 401);
  assert.equal(requests[0].headers["x-memvault-agent-id"], "opencode-other-contract-agent");
});

test("warns when candidates were returned but none were persisted", async (t) => {
  fakeBackend(t, () => jsonResponse({
    memories: [{ content: "candidate" }],
    saved_ids: [],
    warning: "auto_save=false: extracted memories were returned but not persisted",
  }));
  const warnings = captureWarnings(t);
  const { MemVault } = await loadPlugin();
  const plugin = await MemVault({ client: fakeClient("remember this") });
  await runIdle(plugin);
  assert.ok(warnings.some((w) => w.includes("persisted 0")), "a non-persisting response must surface a warning");
});
