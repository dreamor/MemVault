// MemVault extension for the pi agent harness — before-agent-start recall.
//
// The extension targets pi's documented ExtensionAPI: event handlers receive
// (event, ctx), `before_agent_start` returns a CustomMessage wrapper, and
// commands are registered as (name, { description, handler }). Every backend
// failure degrades to a no-op injection.

const BASE = process.env.MEMVAULT_HTTP_URL || "http://127.0.0.1:3777";
const AGENT = process.env.MEMVAULT_AGENT_ID || "pi";

async function recall() {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 5000);
  try {
    const res = await fetch(`${BASE}/api/session?output=plain`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ agent_id: AGENT }),
      signal: controller.signal,
    });
    if (!res.ok) return null;
    const text = await res.text();
    return text.trim() ? text.trim() : null;
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
      if (sessionId !== null) injectedFor = sessionId;
      const memories = await recall();
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
        const list = await fetch(`${BASE}/api/inbox`).then(
          (r) => (r.ok ? r.text() : "inbox unreachable"),
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
