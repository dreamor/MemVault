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
      requests.push({ url: req.url, body: parsed });
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
