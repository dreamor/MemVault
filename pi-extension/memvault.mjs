// MemVault extension for the pi agent harness — session-start recall.
//
// Possibly-fragile points are isolated and marked VERIFY: the extension
// hook name and the injection return shape must be checked against the
// installed pi version (`pi install git:github.com/dreamor/memvault`).
// Every backend failure degrades to a no-op injection.

const BASE = process.env.MEMVAULT_HTTP_URL || "http://127.0.0.1:3777";
const AGENT = process.env.MEMVAULT_AGENT_ID || "pi";

async function recall() {
  try {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 5000);
    const res = await fetch(`${BASE}/api/session?output=plain`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ agent_id: AGENT }),
      signal: controller.signal,
    });
    clearTimeout(timer);
    if (!res.ok) return null;
    const text = await res.text();
    return text.trim() ? text.trim() : null;
  } catch {
    return null;
  }
}

export default function memvault(pi) {
  let injectedFor = null;

  // VERIFY: hook name ("before_agent_start") and the injection return shape.
  // pi's extension contract may differ per release; adjust only here.
  if (typeof pi?.on === "function") {
    pi.on("before_agent_start", async (event) => {
      const sessionId = event?.session?.id ?? null;
      if (sessionId !== null && sessionId === injectedFor) return undefined;
      injectedFor = sessionId;
      const memories = await recall();
      if (!memories) return undefined;
      return { message: memories };
    });
  }

  // VERIFY: command-registration shape when pi confirms it; a no-op guard
  // keeps the extension loadable across versions either way.
  if (typeof pi?.registerCommand === "function") {
    pi.registerCommand("memvault-review", async (ctx) => {
      const list = await fetch(`${BASE}/api/inbox`).then(
        (r) => (r.ok ? r.text() : "inbox unreachable"),
        () => "inbox unreachable",
      );
      return list;
    });
  }
}
