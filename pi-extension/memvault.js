// MemVault extension for the pi agent harness — before-agent-start recall.
//
// The extension targets pi's documented ExtensionAPI: event handlers receive
// (event, ctx), `before_agent_start` returns a CustomMessage wrapper, and
// commands are registered as (name, { description, handler }). Every backend
// failure degrades to a no-op injection — but only until the NEXT agent
// start: a transient failure (e.g. 503 while the backend warms up) must not
// disable recall for the rest of the session (C9).
//
// Auth: deployments that register agent API keys answer 401 without the
// `x-memvault-api-key` header. Set MEMVAULT_API_KEY to authenticate; a 401
// is surfaced on stderr instead of being swallowed like transport errors.

const BASE = process.env.MEMVAULT_HTTP_URL || "http://127.0.0.1:3777";
const AGENT = process.env.MEMVAULT_AGENT_ID || "pi";

function requestHeaders() {
  const headers = {
    "content-type": "application/json",
    "x-memvault-agent-id": AGENT,
  };
  // Read lazily so a key provisioned after startup still takes effect.
  const apiKey = process.env.MEMVAULT_API_KEY || "";
  if (apiKey) headers["x-memvault-api-key"] = apiKey;
  return headers;
}

function warnUnauthorized(what) {
  console.warn(
    `[memvault] ${what} rejected with 401 — the backend requires an API key; ` +
      "set MEMVAULT_API_KEY for this deployment",
  );
}

// Returns the recalled text (possibly "") on success, or null on failure.
// Callers must distinguish the two: success marks the session as served,
// failure leaves it eligible for a retry on the next agent start.
async function recall() {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 5000);
  try {
    const res = await fetch(`${BASE}/api/session?output=plain`, {
      method: "POST",
      headers: requestHeaders(),
      body: JSON.stringify({ agent_id: AGENT }),
      signal: controller.signal,
    });
    if (!res.ok) {
      if (res.status === 401) warnUnauthorized("recall");
      return null;
    }
    const text = await res.text();
    return text.trim();
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
  }
}

export default function memvault(pi) {
  let injectedFor = null;

  if (typeof pi?.on === "function") {
    pi.on("before_agent_start", async (_event, ctx) => {
      const sessionId = ctx?.sessionManager?.getSessionId?.() ?? null;
      if (sessionId !== null && sessionId === injectedFor) return undefined;
      const memories = await recall();
      // C9: mark the session only AFTER a successful recall. Marking before
      // the await meant one failed request (backend restarting, 503) made
      // every later agent start in the same session skip recall entirely.
      if (memories === null) return undefined;
      if (sessionId !== null) injectedFor = sessionId;
      if (!memories) return undefined;
      return {
        message: {
          customType: "memvault-recall",
          content: memories,
          display: true,
        },
      };
    });
  }

  if (typeof pi?.registerCommand === "function") {
    pi.registerCommand("memvault-review", {
      description: "Show the MemVault review inbox",
      handler: async (_args, ctx) => {
        const list = await fetch(`${BASE}/api/inbox`, {
          headers: requestHeaders(),
        }).then(
          (r) => {
            if (r.status === 401) warnUnauthorized("inbox");
            return r.ok ? r.text() : "inbox unreachable";
          },
          () => "inbox unreachable",
        );
        if (ctx?.hasUI && typeof ctx.ui?.notify === "function") {
          ctx.ui.notify(list, "info");
        } else {
          console.log(list);
        }
      },
    });
  }
}
