// MemVault plugin for OpenCode — shared memory for AI agents.
//
// What it does:
// - Injects recalled memories into every session (system prompt transform),
//   so context recovery needs no model initiative.
// - When a session goes idle, hands the last turns to the MemVault REST
//   extractor; drafts land in the review inbox (never straight into context).
// - Degrades silently: if no backend answers, OpenCode keeps working.
//
// Configure in your project's opencode.json (see README.md). Tunables:
//   MEMVAULT_HTTP_URL   REST base, default http://127.0.0.1:3777
//   MEMVAULT_AGENT_ID   agent identity, default "opencode"
//   MEMVAULT_HOOK_EXTRACT  "1" = enable idle-time extraction (default off)

const BASE = process.env.MEMVAULT_HTTP_URL || "http://127.0.0.1:3777";
const AGENT = process.env.MEMVAULT_AGENT_ID || "opencode";
const EXTRACT = process.env.MEMVAULT_HOOK_EXTRACT === "1";
const TIMEOUT_MS = 5000;

async function call(path, body) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), TIMEOUT_MS);
  try {
    const res = await fetch(`${BASE}${path}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
      signal: controller.signal,
    });
    if (!res.ok) return null;
    return await res.text();
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
  }
}

// Plain-text recall (POST /api/session?output=plain) — an empty string means
// "nothing to say", and the transform must not push an empty chunk.
async function recall(contextHint) {
  const text = await call("/api/session?output=plain", {
    agent_id: AGENT,
    context_hint: contextHint || undefined,
  });
  return text && text.trim() ? text.trim() : null;
}

export const MemVault = async ({ client }) => {
  return {
    "experimental.chat.system.transform": async (_input, output) => {
      try {
        const memories = await recall(_input?.contextHint ?? "");
        if (memories) output.system.push(memories);
      } catch {
        // Never let memory break the chat turn.
      }
    },
    event: async ({ event }) => {
      if (!EXTRACT || event.type !== "session.idle") return;
      try {
        const info = event.properties || {};
        const sessionId = info.sessionID || info.sessionId;
        if (!sessionId) return;
        // Pull the last messages straight from the host so no transcript
        // parsing happens in shell/JS — the REST extractor does the rules.
        const messages = (await client.session.messages({ path: { id: sessionId } })).data ?? [];
        const transcript = messages
          .map((message) => message?.parts ?? [])
          .flat()
          .filter((part) => part?.type === "text" && part.text)
          .map((part) => `turn: ${part.text.trim()}`)
          .filter(Boolean)
          .join("\n");
        if (!transcript) return;
        // Extracted drafts are always unreviewed on the server side — the
        // review inbox is the trust boundary, same as every other source.
        await call("/api/extract", {
          text: transcript.slice(-200_000),
          agent_id: AGENT,
          context_hint: sessionId,
        });
      } catch {
        // Extraction is best-effort; the review inbox is fed later.
      }
    },
  };
};
