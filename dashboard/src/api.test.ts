import { describe, it, expect, vi, beforeEach } from "vitest";
import {
  listMemories,
  searchMemories,
  createMemory,
  updateMemory,
  runDedup,
  getInbox,
  health,
  getApiKey,
  setApiKey,
  getAgentId,
  setAgentId,
} from "./api";

const fetchMock = vi.fn();

function jsonResponse(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

const restMemory = {
  id: "mem-1",
  content: "用 Rust 写",
  instruction: null,
  priority: "MUST",
  type: "Preference",
  tags: ["coding"],
  namespace: "global",
  layer: "L1",
  human_reviewed: true,
  ai_generated: false,
  confidence: 0.9,
  access_count: 1,
  decay_score: 0.2,
  created_at: "2026-07-01T00:00:00Z",
  updated_at: "2026-07-01T00:00:00Z",
  source_agent: "alex-code",
  skill_meta: null,
};

function mockSuccess(body: unknown, status = 200) {
  fetchMock.mockResolvedValue(jsonResponse({ ok: true, data: body }, status));
}

/** Return the RequestInit of the fetch call whose URL matches `match`. */
function callFor(match: string | RegExp) {
  const found = fetchMock.mock.calls.find(([input]) => {
    const url = String(input);
    return typeof match === "string" ? url.includes(match) : match.test(url);
  });
  expect(found, `expected a fetch call matching ${match}`).toBeTruthy();
  return found![1] as RequestInit;
}

beforeEach(() => {
  fetchMock.mockReset();
  localStorage.clear();
  vi.stubGlobal("fetch", fetchMock);
});

describe("api.ts field mapping", () => {
  it("listMemories sends limit/offset/namespace and maps to MemoryView", async () => {
    mockSuccess([restMemory]);
    const views = await listMemories({ namespace: "global", limit: 3, offset: 5 });

    const [input, init] = fetchMock.mock.calls[0];
    const url = String(input);
    expect(url).toContain("/api/memories?");
    expect(url).toContain("limit=3");
    expect(url).toContain("offset=5");
    expect(url).toContain("namespace=global");
    expect(init?.method).toBe("GET");

    expect(views).toHaveLength(1);
    expect(views[0].memory_type).toBe("Preference");
    expect(views[0].source_agent_id).toBe("alex-code");
  });

  it("searchMemories translates topK -> top_k and mode into the POST body", async () => {
    mockSuccess([{ memory: restMemory, score: 0.9 }]);
    const results = await searchMemories({ query: "rust", topK: 7, mode: "hybrid" });

    const init = callFor("/api/search");
    expect(init.method).toBe("POST");
    const body = JSON.parse(init.body as string);
    expect(body).toEqual({ query: "rust", top_k: 7, mode: "hybrid" });
    expect(body).not.toHaveProperty("topK");

    expect(results[0].memory.memory_type).toBe("Preference");
    expect(results[0].score).toBe(0.9);
  });

  it("searchMemories forwards namespace when provided", async () => {
    mockSuccess([]);
    await searchMemories({ query: "rust", namespace: "project:x" });

    const init = callFor("/api/search");
    const body = JSON.parse(init.body as string);
    expect(body.namespace).toBe("project:x");
  });

  it("getInbox unwraps {memories, total} and maps to MemoryView", async () => {
    mockSuccess({ memories: [restMemory], total: 1 });
    const views = await getInbox(50);

    const url = String(fetchMock.mock.calls[0][0]);
    expect(url).toContain("/api/inbox");
    expect(url).toContain("limit=50");

    expect(views).toHaveLength(1);
    expect(views[0].id).toBe("mem-1");
    expect(views[0].memory_type).toBe("Preference");
  });

  it("createMemory sends type (not memory_type) with human_reviewed + agent_id", async () => {
    mockSuccess({ id: "mem-2", embedded: false });
    await createMemory({
      content: "记住深色模式",
      priority: "REFERENCE",
      memory_type: "preference",
      namespace: "global",
      tags: ["ui"],
    });

    const init = callFor("/api/memories");
    expect(init.method).toBe("POST");
    const body = JSON.parse(init.body as string);
    expect(body.type).toBe("preference");
    expect(body.memory_type).toBeUndefined();
    expect(body.human_reviewed).toBe(true);
    expect(body.ai_generated).toBe(false);
    expect(body.agent_id).toBe("dashboard");
  });

  it("updateMemory sends type and keeps skill fields nullable", async () => {
    mockSuccess(restMemory);
    await updateMemory("mem-1", {
      content: "改文案",
      priority: "MUST",
      memory_type: "preference",
      namespace: "global",
      tags: [],
      skill_trigger: null,
      skill_steps: null,
      skill_verification: null,
    });

    const init = callFor("/api/memories/mem-1");
    expect(init.method).toBe("PUT");
    const body = JSON.parse(init.body as string);
    expect(body.type).toBe("preference");
    expect(body.memory_type).toBeUndefined();
    expect(body.skill_trigger).toBeNull();
  });

  it("runDedup reshapes {unique, duplicates} to unique_count/duplicate_count", async () => {
    mockSuccess({ unique: 3, duplicates: 1 });
    const result = await runDedup();
    expect(result).toEqual({ unique_count: 3, duplicate_count: 1 });
  });
});

describe("api.ts envelope and credential handling", () => {
  it("throws a readable error when the envelope reports ok=false", async () => {
    fetchMock.mockResolvedValue(jsonResponse({ ok: false, error: "db locked" }, 500));
    await expect(listMemories({ limit: 1, offset: 0 })).rejects.toThrow("db locked");
  });

  it("throws HTTP <status> when the response is not valid JSON", async () => {
    fetchMock.mockResolvedValue(new Response("boom", { status: 502 }));
    await expect(listMemories({ limit: 1, offset: 0 })).rejects.toThrow("HTTP 502");
  });

  it("exposes a default admin agent id and round-trips stored credentials", () => {
    expect(getAgentId()).toBe("admin");
    expect(getApiKey()).toBe("");
    setApiKey("sk-test");
    expect(getApiKey()).toBe("sk-test");
    setApiKey("");
    expect(getApiKey()).toBe("");
    setAgentId("my-agent");
    expect(getAgentId()).toBe("my-agent");
    setAgentId("");
    expect(getAgentId()).toBe("admin");
  });
});

describe("api.ts health", () => {
  it("resolves true on a plain-text ok /health", async () => {
    fetchMock.mockResolvedValue(new Response("ok", { status: 200 }));
    await expect(health()).resolves.toBe(true);
  });

  it("resolves false when the backend is unreachable", async () => {
    fetchMock.mockResolvedValue(new Response("", { status: 500 }));
    await expect(health()).resolves.toBe(false);
  });
});