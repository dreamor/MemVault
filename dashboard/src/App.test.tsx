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

// Re-export for the module to stay a valid ESM test file.
export {};