import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { fileURLToPath } from "node:url";

import memvault from "./memvault.js";

test("the root pi manifest points at this extension", () => {
  const root = JSON.parse(
    readFileSync(new URL("../package.json", import.meta.url), "utf8"),
  );
  assert.deepEqual(root.pi.extensions, ["./pi-extension/memvault.js"]);
  const expected = fileURLToPath(new URL("./memvault.js", import.meta.url));
  const declared = fileURLToPath(
    new URL(`../${root.pi.extensions[0].replace(/^\.\//, "")}`, import.meta.url),
  );
  assert.equal(declared, expected);
});

function fakePi() {
  const handlers = new Map();
  const commands = new Map();
  return {
    handlers,
    commands,
    on(name, handler) {
      handlers.set(name, handler);
    },
    registerCommand(name, options) {
      commands.set(name, options);
    },
  };
}

test("before_agent_start injects a CustomMessage once per session", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });
  globalThis.fetch = async () => ({
    ok: true,
    text: async () => "remember this",
  });

  const pi = fakePi();
  memvault(pi);

  const handler = pi.handlers.get("before_agent_start");
  assert.equal(typeof handler, "function", "must subscribe to before_agent_start");

  const ctx = { sessionManager: { getSessionId: () => "session-1" } };
  const result = await handler({}, ctx);
  assert.deepEqual(result, {
    message: {
      customType: "memvault-recall",
      content: "remember this",
      display: true,
    },
  });

  assert.equal(
    await handler({}, ctx),
    undefined,
    "a session must not receive the same recall twice",
  );
  assert.deepEqual(
    await handler({}, { sessionManager: { getSessionId: () => "session-2" } }),
    {
      message: {
        customType: "memvault-recall",
        content: "remember this",
        display: true,
      },
    },
    "a new session gets its own recall",
  );
});

test("before_agent_start degrades to a no-op when the backend is down", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });
  globalThis.fetch = async () => {
    throw new Error("offline");
  };

  const pi = fakePi();
  memvault(pi);
  assert.equal(
    await pi.handlers.get("before_agent_start")({}, {
      sessionManager: { getSessionId: () => "session-1" },
    }),
    undefined,
  );
});

test("before_agent_start retries recall after a transient failure (C9)", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });
  let calls = 0;
  globalThis.fetch = async () => {
    calls += 1;
    if (calls === 1) return { ok: false, status: 503 };
    return { ok: true, status: 200, text: async () => "remember this" };
  };

  const pi = fakePi();
  memvault(pi);
  const handler = pi.handlers.get("before_agent_start");
  const ctx = { sessionManager: { getSessionId: () => "session-1" } };

  assert.equal(
    await handler({}, ctx),
    undefined,
    "a failed recall degrades to a no-op",
  );
  assert.deepEqual(
    await handler({}, ctx),
    {
      message: {
        customType: "memvault-recall",
        content: "remember this",
        display: true,
      },
    },
    "the next agent start in the same session must retry — one 503 may not disable recall forever",
  );
});

const namedAgent = "pi-contract-agent";
const fakeKey = "fake-pi-api-key";
let moduleCounter = 0;

// Tests in this file run sequentially; every env/global mutation is restored.
async function authFixture(t, handler, env = {}) {
  const envKeys = ["MEMVAULT_HTTP_URL", "MEMVAULT_AGENT_ID", "MEMVAULT_API_KEY"];
  const originalEnv = new Map(envKeys.map((key) => [key, process.env[key]]));
  const originalFetch = globalThis.fetch;
  const originalWarn = console.warn;
  t.after(() => {
    globalThis.fetch = originalFetch;
    console.warn = originalWarn;
    for (const [key, value] of originalEnv) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
  });
  for (const key of envKeys) delete process.env[key];
  Object.assign(process.env, { MEMVAULT_HTTP_URL: "http://memvault.test", ...env });
  const warnings = [];
  console.warn = (...args) => warnings.push(args.join(" "));
  const requests = [];
  globalThis.fetch = async (url, init) => {
    assert.equal(new URL(url).origin, "http://memvault.test");
    const request = {
      url: new URL(url).pathname + new URL(url).search,
      method: init.method ?? "GET",
      headers: init.headers,
      body: init.body ? JSON.parse(init.body) : null,
    };
    const response = handler(request);
    requests.push({ ...request, status: response.status });
    return response;
  };
  const { default: configuredMemvault } = await import(`./memvault.js?test=${++moduleCounter}`);
  const pi = fakePi();
  configuredMemvault(pi);
  return { pi, requests, warnings };
}

function keyedResponse(request) {
  // The inbox authenticates only the headers; there is no JSON actor.
  const actor = request.headers["x-memvault-agent-id"] ?? "admin";
  if (actor !== namedAgent || request.headers["x-memvault-api-key"] !== fakeKey) {
    return new Response("unauthorized", { status: 401 });
  }
  if (request.url === "/api/session?output=plain") {
    assert.equal(request.method, "POST");
    assert.equal(request.body.agent_id, actor);
    return new Response("remember this");
  }
  assert.equal(request.url, "/api/inbox");
  assert.equal(request.method, "GET");
  assert.equal(request.body, null);
  return new Response("pending memory");
}

test("named agent headers authorize recall and inbox with a lazily provisioned key", async (t) => {
  const { pi, requests } = await authFixture(t, keyedResponse, { MEMVAULT_AGENT_ID: namedAgent });
  process.env.MEMVAULT_API_KEY = fakeKey;
  const result = await pi.handlers.get("before_agent_start")({}, {
    sessionManager: { getSessionId: () => "session-1" },
  });
  assert.equal(result.message.content, "remember this");
  const notices = [];
  await pi.commands.get("memvault-review").handler("", {
    hasUI: true,
    ui: { notify: (message) => notices.push(message) },
  });
  assert.deepEqual(notices, ["pending memory"]);
  assert.deepEqual(requests.map((r) => r.url), ["/api/session?output=plain", "/api/inbox"]);
  for (const request of requests) {
    assert.equal(request.status, 200);
    assert.equal(request.headers["x-memvault-agent-id"], namedAgent);
    assert.equal(request.headers["x-memvault-api-key"], fakeKey);
  }
});

test("default actor is pi for recall and inbox without a key", async (t) => {
  const { pi, requests } = await authFixture(t, () => new Response("remember this"));
  await pi.handlers.get("before_agent_start")({}, {});
  await pi.commands.get("memvault-review").handler("", {
    hasUI: true,
    ui: { notify() {} },
  });
  assert.equal(requests.length, 2);
  assert.equal(requests[0].body.agent_id, "pi");
  for (const request of requests) {
    assert.equal(request.headers["x-memvault-agent-id"], "pi");
    assert.equal(request.headers["x-memvault-api-key"], undefined);
  }
});

test("401 for a wrong named-agent key leaves recall retryable and reports inbox unreachable", async (t) => {
  const { pi, requests, warnings } = await authFixture(t, keyedResponse, {
    MEMVAULT_AGENT_ID: namedAgent,
    MEMVAULT_API_KEY: "fake-wrong-api-key",
  });
  const handler = pi.handlers.get("before_agent_start");
  const ctx = { sessionManager: { getSessionId: () => "session-1" } };
  assert.equal(await handler({}, ctx), undefined);
  assert.equal(await handler({}, ctx), undefined);
  const notices = [];
  const reviewCtx = { hasUI: true, ui: { notify: (message) => notices.push(message) } };
  await pi.commands.get("memvault-review").handler("", reviewCtx);
  assert.deepEqual(requests.map((r) => r.status), [401, 401, 401]);
  assert.deepEqual(notices, ["inbox unreachable"]);
  assert.ok(warnings.some((w) => w.includes("recall rejected with 401")));
  assert.ok(warnings.some((w) => w.includes("inbox rejected with 401")));

  process.env.MEMVAULT_API_KEY = fakeKey;
  assert.equal((await handler({}, ctx)).message.content, "remember this");
  assert.equal(await handler({}, ctx), undefined, "only success marks the session injected");
  await pi.commands.get("memvault-review").handler("", reviewCtx);
  assert.deepEqual(requests.map((r) => r.status), [401, 401, 401, 200, 200]);
  assert.deepEqual(notices, ["inbox unreachable", "pending memory"]);
});

test("a valid key for another actor is rejected on recall and inbox", async (t) => {
  const { pi, requests } = await authFixture(t, keyedResponse, {
    MEMVAULT_AGENT_ID: "pi-other-contract-agent",
    MEMVAULT_API_KEY: fakeKey,
  });
  assert.equal(await pi.handlers.get("before_agent_start")({}, {}), undefined);
  const notices = [];
  await pi.commands.get("memvault-review").handler("", {
    hasUI: true,
    ui: { notify: (message) => notices.push(message) },
  });
  assert.deepEqual(requests.map((r) => r.status), [401, 401]);
  assert.deepEqual(notices, ["inbox unreachable"]);
});

test("registerCommand uses pi's options shape and notifies the UI", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = originalFetch;
  });
  globalThis.fetch = async () => ({
    ok: true,
    text: async () => "pending memory",
  });

  const pi = fakePi();
  memvault(pi);

  const command = pi.commands.get("memvault-review");
  assert.equal(typeof command, "object", "registerCommand takes an options object");
  assert.equal(typeof command.handler, "function");
  assert.ok(command.description);

  const notices = [];
  await command.handler("", {
    hasUI: true,
    ui: { notify: (message, level) => notices.push({ message, level }) },
  });
  assert.deepEqual(notices, [{ message: "pending memory", level: "info" }]);
});
