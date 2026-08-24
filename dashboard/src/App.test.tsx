import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "./App";

const fetchMock = vi.fn();

const emptyStats = {
  total: 0,
  must_count: 0,
  reference_count: 0,
  reviewed_count: 0,
  agents: [],
  namespaces: [],
  layers: { l0: 0, l1: 0, l2: 0, l3: 0 },
  skills: 0,
};

function pendingMemory(id: string, content: string) {
  return {
    id,
    content,
    instruction: null,
    priority: "REFERENCE",
    type: "Fact",
    tags: [],
    namespace: "global",
    layer: "L1",
    human_reviewed: false,
    ai_generated: true,
    confidence: 0.5,
    access_count: 0,
    decay_score: 0,
    created_at: "2026-07-01T00:00:00Z",
    updated_at: "2026-07-01T00:00:00Z",
    source_agent: "claude-code",
    skill_meta: null,
  };
}

function jsonResponse(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function mockFetchDefaults() {
  fetchMock.mockImplementation((input: RequestInfo | URL) => {
    const url = String(input);
    if (url.endsWith("/health")) {
      return Promise.resolve(new Response("ok", { status: 200 }));
    }
    if (url.includes("/api/memories")) {
      return Promise.resolve(jsonResponse({ ok: true, data: [] }));
    }
    if (url.includes("/api/inbox")) {
      return Promise.resolve(jsonResponse({ ok: true, data: { memories: [], total: 0 } }));
    }
    if (url.includes("/api/stats")) {
      return Promise.resolve(jsonResponse({ ok: true, data: emptyStats }));
    }
    if (url.includes("/api/compliance/summary")) {
      return Promise.resolve(
        jsonResponse(
          { ok: false, error: "Compliance tracking is not enabled" },
          500,
        ),
      );
    }
    return Promise.resolve(jsonResponse({ ok: true, data: undefined }));
  });
}

beforeEach(() => {
  fetchMock.mockReset();
  localStorage.clear();
  mockFetchDefaults();
  vi.stubGlobal("fetch", fetchMock);
});

describe("App", () => {
  it("renders the Memories tab with an empty state by default", async () => {
    render(<App />);
    expect(await screen.findByText(/No memories stored yet/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Memories/ })).toHaveClass("active");
  });

  it("switches to the Settings tab and shows the connection status", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByText(/No memories stored yet/);

    await user.click(screen.getByRole("button", { name: "Settings" }));
    expect(await screen.findByText(/Connected/)).toBeInTheDocument();
  });

  it("shows an Unreachable status when the backend /health fails", async () => {
    fetchMock.mockImplementation((input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/health")) {
        return Promise.resolve(new Response("", { status: 503 }));
      }
      return Promise.resolve(jsonResponse({ ok: true, data: undefined }));
    });

    const user = userEvent.setup();
    render(<App />);
    await screen.findByText(/No memories stored yet/);

    await user.click(screen.getByRole("button", { name: "Settings" }));
    expect(await screen.findByText(/Unreachable/)).toBeInTheDocument();
  });

  it("submits the New Memory form via POST /api/memories", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByText(/No memories stored yet/);

    await user.click(screen.getByRole("button", { name: "+ New Memory" }));
    // The form's textareas aren't associated with their <label> via htmlFor/id,
    // so fall back to a DOM query instead of getByLabelText.
    const contentBox = document.querySelector(".detail-panel textarea");
    expect(contentBox).not.toBeNull();
    await user.type(contentBox as Element, "test memory");

    await user.click(screen.getByRole("button", { name: "Create" }));

    await waitFor(() => {
      const createCall = fetchMock.mock.calls.find(
        ([input, init]) =>
          String(input).endsWith("/api/memories") &&
          (init as RequestInit)?.method === "POST",
      );
      expect(createCall).toBeTruthy();
      const body = JSON.parse((createCall![1] as RequestInit).body as string);
      expect(body.content).toBe("test memory");
      expect(body.type).toBe("fact");
      expect(body.human_reviewed).toBe(true);
      expect(body.agent_id).toBe("dashboard");
    });
  });

  it("discards a stale /api/memories response that resolves after a newer one", async () => {
    // Defer /api/memories fetches so we can resolve them out of order.
    const resolvers: Array<(r: Response) => void> = [];
    fetchMock.mockImplementation((input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("/api/memories")) {
        let resolve!: (r: Response) => void;
        const p = new Promise<Response>((res) => (resolve = res));
        resolvers.push(resolve);
        return p;
      }
      if (url.endsWith("/health")) {
        return Promise.resolve(new Response("ok", { status: 200 }));
      }
      if (url.includes("/api/inbox")) {
        return Promise.resolve(jsonResponse({ ok: true, data: { memories: [], total: 0 } }));
      }
      if (url.includes("/api/stats")) {
        return Promise.resolve(jsonResponse({ ok: true, data: emptyStats }));
      }
      if (url.includes("/api/compliance/summary")) {
        return Promise.resolve(jsonResponse({ ok: false, error: "not enabled" }, 500));
      }
      return Promise.resolve(jsonResponse({ ok: true, data: undefined }));
    });

    const memory = (id: string, content: string) => ({
      id,
      content,
      instruction: null,
      priority: "REFERENCE",
      type: "Fact",
      tags: [],
      namespace: "global",
      layer: "L1",
      human_reviewed: true,
      ai_generated: false,
      confidence: 0.9,
      access_count: 0,
      decay_score: 0,
      created_at: "2026-07-01T00:00:00Z",
      updated_at: "2026-07-01T00:00:00Z",
      source_agent: "alex-code",
      skill_meta: null,
    });

    const user = userEvent.setup();
    render(<App />);

    // First loadMemories on mount.
    await waitFor(() => expect(resolvers.length).toBe(1));

    // Re-enter the Memories tab to trigger a second, newer request.
    await user.click(screen.getByRole("button", { name: /Review/ }));
    await user.click(screen.getByRole("button", { name: /Memories/ }));
    await waitFor(() => expect(resolvers.length).toBe(2));

    // The newer request resolves first.
    await act(async () => {
      resolvers[1](jsonResponse({ ok: true, data: [memory("mem-b", "最新记忆 B")] }));
    });
    expect(await screen.findByText("最新记忆 B")).toBeInTheDocument();

    // The older request resolves late — its result must NOT overwrite B.
    await act(async () => {
      resolvers[0](jsonResponse({ ok: true, data: [memory("mem-a", "过期记忆 A")] }));
    });
    expect(screen.queryByText("过期记忆 A")).not.toBeInTheDocument();
    expect(screen.getByText("最新记忆 B")).toBeInTheDocument();
  });
});

  it("refreshes the Review badge when the window regains focus", async () => {
    render(<App />);
    await screen.findByText(/No memories stored yet/);
    expect(screen.getByRole("button", { name: /Review \(0\)/ })).toBeInTheDocument();

    // An agent writes a memory via CLI/MCP while this tab is in the background.
    fetchMock.mockImplementation((input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("/api/inbox")) {
        return Promise.resolve(
          jsonResponse({
            ok: true,
            data: { memories: [pendingMemory("mem-p1", "待评审记忆")], total: 1 },
          }),
        );
      }
      if (url.endsWith("/health")) {
        return Promise.resolve(new Response("ok", { status: 200 }));
      }
      if (url.includes("/api/memories")) {
        return Promise.resolve(jsonResponse({ ok: true, data: [] }));
      }
      if (url.includes("/api/stats")) {
        return Promise.resolve(jsonResponse({ ok: true, data: emptyStats }));
      }
      return Promise.resolve(jsonResponse({ ok: true, data: undefined }));
    });

    // Returning to the page triggers a focus event -> badge must refresh.
    await act(async () => {
      window.dispatchEvent(new Event("focus"));
    });

    expect(await screen.findByRole("button", { name: /Review \(1\)/ })).toBeInTheDocument();
  });

  it("polls the backend on an interval to keep header badges fresh", async () => {
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
    try {
      render(<App />);
      await screen.findByText(/No memories stored yet/);

      const inboxCalls = () =>
        fetchMock.mock.calls.filter(([u]) => String(u).includes("/api/inbox")).length;
      const before = inboxCalls();
      expect(before).toBeGreaterThanOrEqual(1);

      // One poll tick: inbox (Review badge) must be fetched again.
      await act(async () => {
        vi.advanceTimersByTime(30_000);
      });

      await waitFor(() => {
        expect(inboxCalls()).toBeGreaterThan(before);
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it("sends the selected search mode to POST /api/search and shows hit sources", async () => {
    const user = userEvent.setup();
    fetchMock.mockImplementation((input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/api/search")) {
        // Echo the requested mode so we can assert it was passed through (the
        // actual request body is asserted from fetchMock.mock.calls below).
        return Promise.resolve(
          jsonResponse({
            ok: true,
            data: [
              {
                memory: { ...pendingMemory("mem-s1", "deploy memvault via docker"), human_reviewed: true },
                score: 0.81,
                search_mode: "hybrid",
                hit_sources: ["kw#1", "vec#2"],
              },
            ],
          }),
        );
      }
      if (url.endsWith("/health")) return Promise.resolve(new Response("ok", { status: 200 }));
      if (url.includes("/api/memories")) return Promise.resolve(jsonResponse({ ok: true, data: [] }));
      if (url.includes("/api/inbox")) return Promise.resolve(jsonResponse({ ok: true, data: { memories: [], total: 0 } }));
      if (url.includes("/api/stats")) return Promise.resolve(jsonResponse({ ok: true, data: emptyStats }));
      return Promise.resolve(jsonResponse({ ok: true, data: undefined }));
    });

    render(<App />);
    await screen.findByText(/No memories stored yet/);

    await user.click(screen.getByRole("button", { name: "Search" }));
    await user.selectOptions(screen.getByLabelText("Search mode"), "hybrid");
    await user.type(screen.getByPlaceholderText("Search memories..."), "docker");
    await user.click(screen.getByRole("button", { name: "Run search" }));

    await waitFor(() => {
      const searchCall = fetchMock.mock.calls.find(
        ([input]) => String(input).endsWith("/api/search"),
      );
      expect(searchCall).toBeTruthy();
      const body = JSON.parse((searchCall![1] as RequestInit).body as string);
      expect(body.mode).toBe("hybrid");
      expect(body.query).toBe("docker");
    });

    // Backend-echoed mode and provenance tags are rendered on the card.
    expect(await screen.findByText("kw#1 vec#2")).toBeInTheDocument();
  });

  it("highlights matching query terms inside search results", async () => {
    const user = userEvent.setup();
    fetchMock.mockImplementation((input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/api/search")) {
        return Promise.resolve(
          jsonResponse({
            ok: true,
            data: [
              {
                memory: { ...pendingMemory("mem-h1", "run docker build, then deploy"), human_reviewed: true },
                score: 0.9,
                search_mode: "keyword",
                hit_sources: ["kw#1"],
              },
            ],
          }),
        );
      }
      if (url.endsWith("/health")) return Promise.resolve(new Response("ok", { status: 200 }));
      if (url.includes("/api/memories")) return Promise.resolve(jsonResponse({ ok: true, data: [] }));
      if (url.includes("/api/inbox")) return Promise.resolve(jsonResponse({ ok: true, data: { memories: [], total: 0 } }));
      if (url.includes("/api/stats")) return Promise.resolve(jsonResponse({ ok: true, data: emptyStats }));
      return Promise.resolve(jsonResponse({ ok: true, data: undefined }));
    });

    render(<App />);
    await screen.findByText(/No memories stored yet/);

    await user.click(screen.getByRole("button", { name: "Search" }));
    await user.type(screen.getByPlaceholderText("Search memories..."), "docker");
    await user.click(screen.getByRole("button", { name: "Run search" }));

    await waitFor(() => {
      const marks = document.querySelectorAll("mark");
      expect(marks.length).toBeGreaterThan(0);
      expect(marks[0].textContent).toBe("docker");
    });
  });


// Re-export for the module to stay a valid ESM test file.
export {};