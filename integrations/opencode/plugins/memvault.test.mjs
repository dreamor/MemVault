// Behavioural test for the OpenCode idle-extraction path.
//
// Regression (L2): the plugin used to POST only { text, agent_id,
// context_hint } to /api/extract. The endpoint defaults `auto_save` to
// false and records provenance from `source_id`, so drafts were returned to
// the plugin and then dropped — the review inbox never grew. This test
// drives the real plugin handler against a fake REST server and asserts the
// request actually asks for persistence.

import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";

let moduleCounter = 0;

async function fakeServer(handler) {
  const requests = [];
  const server = http.createServer((req, res) => {
    let body = "";
    req.on("data", (chunk) => (body += chunk));
    req.on("end", () => {
      const parsed = body ? JSON.parse(body) : null;
      requests.push({ url: req.url, body: parsed, headers: req.headers });
      const payload = handler(req, parsed);
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify(payload));
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return { server, requests, base: `http://127.0.0.1:${server.address().port}` };
}

// The plugin reads MEMVAULT_* at module load; a unique query string gives
// each test a fresh module instance after the env has been pointed at its
// own fake server.
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

test("session.idle extraction asks the server to persist the draft", async () => {
  const { server, requests, base } = await fakeServer(() =>
    jsonResponse({
      memories: [{ content: "I always prefer dark mode" }],
      saved_ids: ["mem_1"],
      warning: null,
    }),
  );
  process.env.MEMVAULT_HTTP_URL = base;
  process.env.MEMVAULT_HOOK_EXTRACT = "1";
  process.env.MEMVAULT_AGENT_ID = "opencode";
  try {
    const { MemVault } = await loadPlugin();
    const plugin = await MemVault({ client: fakeClient("I always prefer dark mode") });
    await runIdle(plugin);

    const extract = requests.find((r) => r.url === "/api/extract");
    assert.ok(extract, "idle event must call /api/extract");
    assert.equal(extract.body.auto_save, true, "auto_save must opt into persistence");
    assert.equal(extract.body.source_id, "opencode", "provenance uses source_id");
    assert.equal(extract.body.agent_id, "opencode");
    assert.match(extract.body.text, /dark mode/);
  } finally {
    server.close();
    delete process.env.MEMVAULT_HTTP_URL;
    delete process.env.MEMVAULT_HOOK_EXTRACT;
    delete process.env.MEMVAULT_AGENT_ID;
  }
});

test("repeated idles extract only messages newer than the last run (C10)", async () => {
  const { server, requests, base } = await fakeServer(() =>
    jsonResponse({ memories: [], saved_ids: [], warning: null }),
  );
  process.env.MEMVAULT_HTTP_URL = base;
  process.env.MEMVAULT_HOOK_EXTRACT = "1";
  process.env.MEMVAULT_AGENT_ID = "opencode";
  try {
    const all = [{ parts: [{ type: "text", text: "first turn text" }] }];
    const client = { session: { messages: async () => ({ data: all }) } };
    const { MemVault } = await loadPlugin();
    const plugin = await MemVault({ client });

    await runIdle(plugin);
    await runIdle(plugin); // nothing new since the last extraction
    let extracts = requests.filter((r) => r.url === "/api/extract");
    assert.equal(
      extracts.length,
      1,
      "an idle with no new messages must not re-send the transcript",
    );

    all.push({ parts: [{ type: "text", text: "second turn text" }] });
    await runIdle(plugin);
    extracts = requests.filter((r) => r.url === "/api/extract");
    assert.equal(extracts.length, 2);
    assert.match(extracts[1].body.text, /second turn text/);
    assert.doesNotMatch(
      extracts[1].body.text,
      /first turn text/,
      "already-extracted turns must not be re-sent (duplicate inbox drafts)",
    );
  } finally {
    server.close();
    delete process.env.MEMVAULT_HTTP_URL;
    delete process.env.MEMVAULT_HOOK_EXTRACT;
    delete process.env.MEMVAULT_AGENT_ID;
  }
});

test("a failed extraction is retried at the next idle (C10)", async () => {
  let fail = true;
  const { server, requests, base } = await fakeServer(() => {
    if (fail) return { ok: false, error: "backend down" };
    return jsonResponse({ memories: [], saved_ids: [], warning: null });
  });
  process.env.MEMVAULT_HTTP_URL = base;
  process.env.MEMVAULT_HOOK_EXTRACT = "1";
  process.env.MEMVAULT_AGENT_ID = "opencode";
  try {
    const { MemVault } = await loadPlugin();
    const plugin = await MemVault({ client: fakeClient("remember this") });
    await runIdle(plugin); // server answers ok:false → watermark must not advance
    fail = false;
    await runIdle(plugin);
    const extracts = requests.filter((r) => r.url === "/api/extract");
    assert.equal(extracts.length, 2, "the failed tail must be retried");
    assert.match(extracts[1].body.text, /remember this/);
  } finally {
    server.close();
    delete process.env.MEMVAULT_HTTP_URL;
    delete process.env.MEMVAULT_HOOK_EXTRACT;
    delete process.env.MEMVAULT_AGENT_ID;
  }
});

test("requests carry the API key header when MEMVAULT_API_KEY is set (C12)", async () => {
  const { server, requests, base } = await fakeServer(() =>
    jsonResponse({ memories: [], saved_ids: [], warning: null }),
  );
  process.env.MEMVAULT_HTTP_URL = base;
  process.env.MEMVAULT_HOOK_EXTRACT = "1";
  process.env.MEMVAULT_AGENT_ID = "opencode";
  process.env.MEMVAULT_API_KEY = "test-key-456";
  try {
    const { MemVault } = await loadPlugin();
    const plugin = await MemVault({ client: fakeClient("remember this") });
    await runIdle(plugin);
    const extract = requests.find((r) => r.url === "/api/extract");
    assert.ok(extract);
    assert.equal(
      extract.headers["x-memvault-api-key"],
      "test-key-456",
      "keyed deployments 401 without the header",
    );
  } finally {
    server.close();
    delete process.env.MEMVAULT_HTTP_URL;
    delete process.env.MEMVAULT_HOOK_EXTRACT;
    delete process.env.MEMVAULT_AGENT_ID;
    delete process.env.MEMVAULT_API_KEY;
  }
});

test("warns when candidates were returned but none were persisted", async () => {
  const { server, base } = await fakeServer(() =>
    jsonResponse({
      memories: [{ content: "candidate" }],
      saved_ids: [],
      warning: "auto_save=false: extracted memories were returned but not persisted",
    }),
  );
  process.env.MEMVAULT_HTTP_URL = base;
  process.env.MEMVAULT_HOOK_EXTRACT = "1";
  process.env.MEMVAULT_AGENT_ID = "opencode";
  const warnings = [];
  const originalWarn = console.warn;
  console.warn = (...args) => warnings.push(args.join(" "));
  try {
    const { MemVault } = await loadPlugin();
    const plugin = await MemVault({ client: fakeClient("remember this") });
    await runIdle(plugin);
    assert.ok(
      warnings.some((w) => w.includes("persisted 0")),
      `a non-persisting server must surface a warning, got: ${warnings.join(" | ")}`,
    );
  } finally {
    console.warn = originalWarn;
    server.close();
    delete process.env.MEMVAULT_HTTP_URL;
    delete process.env.MEMVAULT_HOOK_EXTRACT;
    delete process.env.MEMVAULT_AGENT_ID;
  }
});
