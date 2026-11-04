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
//   MEMVAULT_API_KEY    sent as `x-memvault-api-key`; required by
//                       deployments that register agent API keys (without
//                       it every call 401s — surfaced on stderr, not
//                       swallowed like transport errors)

const BASE = process.env.MEMVAULT_HTTP_URL || "http://127.0.0.1:3777";
const AGENT = process.env.MEMVAULT_AGENT_ID || "opencode";
const EXTRACT = process.env.MEMVAULT_HOOK_EXTRACT === "1";
const TIMEOUT_MS = 5000;

function requestHeaders() {
  const headers = { "content-type": "application/json" };
  // Read lazily so a key provisioned after startup still takes effect.
  const apiKey = process.env.MEMVAULT_API_KEY || "";
  if (apiKey) headers["x-memvault-api-key"] = apiKey;
  return headers;
}

async function call(path, body) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), TIMEOUT_MS);
  try {
    const res = await fetch(`${BASE}${path}`, {
      method: "POST",
      headers: requestHeaders(),
      body: JSON.stringify(body),
      signal: controller.signal,
    });
    if (!res.ok) {
      if (res.status === 401) {
        console.warn(
          `[memvault] ${path} rejected with 401 — the backend requires an API key; ` +
            "set MEMVAULT_API_KEY for this deployment",
        );
      }
      return null;
    }
    return await res.text();
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
  }
}

// JSON helper for endpoints that wrap their payload in { ok, data, error }.
// Returns the unwrapped data, or null on any transport/HTTP/parse failure —
// callers stay best-effort but can still tell "nothing saved" from "saved".
async function postJson(path, body) {
  const text = await call(path, body);
  if (!text) return null;
  try {
    const parsed = JSON.parse(text);
    if (parsed && typeof parsed === "object" && "ok" in parsed) {
      return parsed.ok ? parsed.data : null;
    }
    return parsed;
  } catch {
    return null;
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
  // Idle-extraction watermark (C10): `session.idle` fires EVERY time a
  // session goes quiet — without a watermark each firing re-sent the whole
  // transcript, so the extractor drafted the same turns over and over and
  // the review inbox filled with duplicates. Track how many messages were
  // already extracted per session and only send the tail beyond that. The
  // watermark advances only on a successful POST, so a failed extraction
  // retries at the next idle instead of being lost.
  const extractedCounts = new Map();
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
        const alreadyExtracted = extractedCounts.get(sessionId) ?? 0;
        if (messages.length <= alreadyExtracted) return;
        const transcript = messages
          .slice(alreadyExtracted)
          .map((message) => message?.parts ?? [])
          .flat()
          .filter((part) => part?.type === "text" && part.text)
          .map((part) => `turn: ${part.text.trim()}`)
          .filter(Boolean)
          .join("\n");
        if (!transcript) return;
        // Extracted drafts are always unreviewed on the server side — the
        // review inbox is the trust boundary, same as every other source.
        // `auto_save` is what actually persists them (the endpoint defaults
        // to extract-only), and `source_id` — not `agent_id` — is the field
        // the server records as the saved memory's provenance.
        const result = await postJson("/api/extract", {
          text: transcript.slice(-200_000),
          agent_id: AGENT,
          source_id: AGENT,
          auto_save: true,
          context_hint: sessionId,
        });
        // Advance the watermark only on a successful call — a failed POST
        // leaves the tail unextracted so the next idle retries it.
        if (result) extractedCounts.set(sessionId, messages.length);
        // A contract regression must be visible instead of looking like a
        // successful extraction that silently fed nothing into the inbox.
        const candidates = result?.memories?.length ?? 0;
        const saved = result?.saved_ids?.length ?? 0;
        if (!result || result.warning || (candidates > 0 && saved === 0)) {
          console.warn(
            `[memvault] extraction returned ${candidates} candidate(s) but persisted ${saved}`,
            result?.warning ?? (result ? "" : "request failed"),
          );
        }
      } catch {
        // Extraction is best-effort; the review inbox is fed later.
      }
    },
  };
};
