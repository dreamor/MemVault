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

test("requests carry the API key header when MEMVAULT_API_KEY is set (C12)", async (t) => {
  const originalFetch = globalThis.fetch;
  const originalKey = process.env.MEMVAULT_API_KEY;
  t.after(() => {
    globalThis.fetch = originalFetch;
    if (originalKey === undefined) delete process.env.MEMVAULT_API_KEY;
    else process.env.MEMVAULT_API_KEY = originalKey;
  });
  process.env.MEMVAULT_API_KEY = "test-key-123";

  const seen = [];
  globalThis.fetch = async (_url, init) => {
    seen.push(init?.headers);
    return { ok: true, status: 200, text: async () => "remember this" };
  };

  const pi = fakePi();
  memvault(pi);
  await pi.handlers.get("before_agent_start")(
    {},
    { sessionManager: { getSessionId: () => "session-1" } },
  );
  assert.equal(seen.length, 1);
  assert.equal(seen[0]["x-memvault-api-key"], "test-key-123");
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
