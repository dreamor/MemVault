import { Blob } from "node:buffer";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "./App";

const fetchMock = vi.fn();

/** Return the RequestInit of the fetch call whose URL contains `match`. */
function callFor(match: string) {
  const found = fetchMock.mock.calls.find((call: unknown[]) => String(call[0]).includes(match));
  expect(found, `expected a fetch call matching ${match}`).toBeTruthy();
  return found![1] as RequestInit;
}

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
    if (url.includes("/api/episodes")) {
      return Promise.resolve(jsonResponse({ ok: true, data: { episodes: [], count: 0 } }));
    }
    if (url.includes("/api/outcome")) {
      return Promise.resolve(
        jsonResponse({
          ok: true,
          data: { id: "mem_new", outcome: "[success] recorded", embedded: false, lesson: null },
        }),
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
describe("App — stats tab", () => {
  it("renders the stats grid and pipeline actions from /api/stats", async () => {
    render(<App />);
    await screen.findByText(/No memories stored yet/);

    await act(async () => {
      screen.getByRole("button", { name: /^Stats$/ }).click();
    });

    expect(await screen.findByText("Total Memories")).toBeInTheDocument();
    expect(screen.getByText("MUST Rules")).toBeInTheDocument();
    expect(screen.getByText("References")).toBeInTheDocument();
    expect(screen.getByText("Skills")).toBeInTheDocument();
    expect(screen.getByText(/No agents have written memories yet/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Run Promote (L1→L2→L3)" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Run Decay" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Run Dedup" })).toBeInTheDocument();
  });

  it("shows the compliance error message when tracking is disabled", async () => {
    render(<App />);
    await screen.findByText(/No memories stored yet/);
    await act(async () => {
      screen.getByRole("button", { name: /^Stats$/ }).click();
    });
    expect(
      await screen.findByText(/Compliance tracking is not enabled on this database/),
    ).toBeInTheDocument();
  });

  it("calls the promote/decay/dedup endpoints from the pipeline actions", async () => {
    const alertSpy = vi.spyOn(window, "alert").mockImplementation(() => {});
    fetchMock.mockImplementation((input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/health")) return Promise.resolve(new Response("ok", { status: 200 }));
      if (url.includes("/api/memories")) return Promise.resolve(jsonResponse({ ok: true, data: [] }));
      if (url.includes("/api/inbox")) {
        return Promise.resolve(jsonResponse({ ok: true, data: { memories: [], total: 0 } }));
      }
      if (url.includes("/api/stats")) {
        return Promise.resolve(jsonResponse({ ok: true, data: emptyStats }));
      }
      if (url.includes("/api/promote")) {
        return Promise.resolve(jsonResponse({ ok: true, data: { promoted_to_l2: 1, promoted_to_l3: 2 } }));
      }
      if (url.includes("/api/decay")) {
        return Promise.resolve(jsonResponse({ ok: true, data: { updated: 3, archived: 1 } }));
      }
      if (url.includes("/api/dedup")) {
        return Promise.resolve(jsonResponse({ ok: true, data: { unique: 5, duplicates: 2 } }));
      }
      if (url.includes("/api/compliance/summary")) {
        return Promise.resolve(jsonResponse({ ok: false, error: "Compliance tracking is not enabled" }, 500));
      }
      return Promise.resolve(jsonResponse({ ok: true, data: undefined }));
    });

    render(<App />);
    await screen.findByText(/No memories stored yet/);
    await act(async () => {
      screen.getByRole("button", { name: /^Stats$/ }).click();
    });
    await screen.findByText("Total Memories");

    await act(async () => {
      screen.getByRole("button", { name: "Run Promote (L1→L2→L3)" }).click();
    });
    await waitFor(() => expect(alertSpy).toHaveBeenCalledWith("Promote: 1 → L2, 2 → L3"));

    await act(async () => {
      screen.getByRole("button", { name: "Run Decay" }).click();
    });
    await waitFor(() => expect(alertSpy).toHaveBeenCalledWith("Decay: 3 updated, 1 archived"));

    await act(async () => {
      screen.getByRole("button", { name: "Run Dedup" }).click();
    });
    await waitFor(() =>
      expect(alertSpy).toHaveBeenCalledWith("Dedup: 5 unique, 2 duplicates found"),
    );

    const endpoints = fetchMock.mock.calls
      .map(([input]) => String(input))
      .filter((u) => /\/api\/(promote|decay|dedup)$/.test(u));
    expect(endpoints).toEqual(["/api/promote", "/api/decay", "/api/dedup"]);
    alertSpy.mockRestore();
  });
});

describe("App — system tab", () => {
  it("loads capabilities and metrics when the tab is opened", async () => {
    fetchMock.mockImplementation((input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/health")) return Promise.resolve(new Response("ok", { status: 200 }));
      if (url.includes("/api/memories")) return Promise.resolve(jsonResponse({ ok: true, data: [] }));
      if (url.includes("/api/inbox")) {
        return Promise.resolve(jsonResponse({ ok: true, data: { memories: [], total: 0 } }));
      }
      if (url.includes("/api/stats")) {
        return Promise.resolve(jsonResponse({ ok: true, data: emptyStats }));
      }
      if (url.includes("/api/capabilities")) {
        return Promise.resolve(
          jsonResponse({
            ok: true,
            data: [{ name: "关键词检索", available: true, note: "始终可用" }],
          }),
        );
      }
      if (url.endsWith("/metrics")) {
        return Promise.resolve(
          new Response("memvault_memories_saved_total 42\n", { status: 200 }),
        );
      }
      return Promise.resolve(jsonResponse({ ok: true, data: undefined }));
    });

    render(<App />);
    await screen.findByText(/No memories stored yet/);
    await act(async () => {
      screen.getByRole("button", { name: "System" }).click();
    });

    expect(await screen.findByText("关键词检索")).toBeInTheDocument();
    expect(await screen.findByText("42")).toBeInTheDocument();
  });

  it("runs the doctor scan on button click and renders findings grouped by severity", async () => {
    fetchMock.mockImplementation((input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/health")) return Promise.resolve(new Response("ok", { status: 200 }));
      if (url.includes("/api/memories")) return Promise.resolve(jsonResponse({ ok: true, data: [] }));
      if (url.includes("/api/inbox")) {
        return Promise.resolve(jsonResponse({ ok: true, data: { memories: [], total: 0 } }));
      }
      if (url.includes("/api/stats")) {
        return Promise.resolve(jsonResponse({ ok: true, data: emptyStats }));
      }
      if (url.includes("/api/capabilities")) {
        return Promise.resolve(jsonResponse({ ok: true, data: [] }));
      }
      if (url.endsWith("/metrics")) {
        return Promise.resolve(new Response("", { status: 200 }));
      }
      if (url.includes("/api/doctor")) {
        return Promise.resolve(
          jsonResponse({
            ok: true,
            data: {
              total_memories: 3,
              findings: [
                {
                  check: "dangling_superseded_by",
                  severity: "warn",
                  count: 1,
                  items: [{ id: "mem-x", detail: "points at a deleted memory" }],
                },
              ],
            },
          }),
        );
      }
      return Promise.resolve(jsonResponse({ ok: true, data: undefined }));
    });

    render(<App />);
    await screen.findByText(/No memories stored yet/);
    await act(async () => {
      screen.getByRole("button", { name: "System" }).click();
    });
    await screen.findByRole("button", { name: "Run Doctor" });

    await act(async () => {
      screen.getByRole("button", { name: "Run Doctor" }).click();
    });

    expect(await screen.findByText("dangling_superseded_by")).toBeInTheDocument();
    expect(screen.getByText(/points at a deleted memory/)).toBeInTheDocument();
    expect(screen.getByText("warn")).toBeInTheDocument();
  });
});

describe("App — data tab", () => {
  function baseDataTabMock(extra: (url: string) => Response | null) {
    return (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/health")) return Promise.resolve(new Response("ok", { status: 200 }));
      if (url.includes("/api/memories")) return Promise.resolve(jsonResponse({ ok: true, data: [] }));
      if (url.includes("/api/inbox")) {
        return Promise.resolve(jsonResponse({ ok: true, data: { memories: [], total: 0 } }));
      }
      if (url.includes("/api/stats")) {
        return Promise.resolve(jsonResponse({ ok: true, data: emptyStats }));
      }
      const custom = extra(url);
      if (custom) return Promise.resolve(custom);
      return Promise.resolve(jsonResponse({ ok: true, data: undefined }));
    };
  }

  it("imports skills from pasted markdown and renders the result", async () => {
    const user = userEvent.setup();
    fetchMock.mockImplementation(
      baseDataTabMock((url) => {
        if (url.includes("/api/skills/import")) {
          return jsonResponse({
            ok: true,
            data: { imported: [{ title: "Deploy", id: "mem-skill-1", steps: 2 }], skipped_no_steps: 0 },
          });
        }
        return null;
      }),
    );

    render(<App />);
    await screen.findByText(/No memories stored yet/);
    await act(async () => {
      screen.getByRole("button", { name: "Data" }).click();
    });

    const textarea = screen.getByPlaceholderText(/Deploy the dashboard/);
    await user.type(textarea, "# Deploy\n1. a\n2. b\n");
    await act(async () => {
      screen.getByRole("button", { name: "Import Skills" }).click();
    });

    expect(await screen.findByText(/Deploy \(2 steps\)/)).toBeInTheDocument();
    expect(callFor("/api/skills/import").method).toBe("POST");
  });

  it("exports memories and triggers a file download", async () => {
    const createObjectURL = vi.fn(() => "blob:mock-url");
    const revokeObjectURL = vi.fn();
    vi.stubGlobal("URL", { ...URL, createObjectURL, revokeObjectURL });
    const clickSpy = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});

    fetchMock.mockImplementation(
      baseDataTabMock((url) => {
        if (url.includes("/api/export")) {
          return jsonResponse({ ok: true, data: { format: "json", content: "[]" } });
        }
        return null;
      }),
    );

    render(<App />);
    await screen.findByText(/No memories stored yet/);
    await act(async () => {
      screen.getByRole("button", { name: "Data" }).click();
    });
    await act(async () => {
      screen.getByRole("button", { name: "Export" }).click();
    });

    await waitFor(() => expect(createObjectURL).toHaveBeenCalled());
    expect(clickSpy).toHaveBeenCalled();
    expect(revokeObjectURL).toHaveBeenCalledWith("blob:mock-url");

    clickSpy.mockRestore();
  });

  it("creates a backup and triggers a file download", async () => {
    const createObjectURL = vi.fn(() => "blob:mock-url");
    const revokeObjectURL = vi.fn();
    vi.stubGlobal("URL", { ...URL, createObjectURL, revokeObjectURL });
    const clickSpy = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});

    fetchMock.mockImplementation(
      baseDataTabMock((url) => {
        if (url.endsWith("/api/backup")) {
          return new Response(new Blob(["fake db bytes"]), {
            status: 200,
            headers: { "content-disposition": 'attachment; filename="memvault-backup-x.db"' },
          });
        }
        return null;
      }),
    );

    render(<App />);
    await screen.findByText(/No memories stored yet/);
    await act(async () => {
      screen.getByRole("button", { name: "Data" }).click();
    });
    await act(async () => {
      screen.getByRole("button", { name: "Create Backup" }).click();
    });

    await waitFor(() => expect(createObjectURL).toHaveBeenCalled());
    expect(clickSpy).toHaveBeenCalled();

    clickSpy.mockRestore();
  });

  it("scans, previews, and imports from a detected agent", async () => {
    fetchMock.mockImplementation(
      baseDataTabMock((url) => {
        if (url.includes("/api/agents/import/scan")) {
          return jsonResponse({
            ok: true,
            data: [
              { agent_key: "codex", display_name: "OpenAI Codex CLI", found: true, paths: ["/tmp/AGENTS.md"] },
              { agent_key: "claude", display_name: "Claude Code", found: false, paths: [] },
            ],
          });
        }
        if (url.includes("/api/agents/import/preview")) {
          return jsonResponse({
            ok: true,
            data: {
              agent_key: "codex",
              display_name: "OpenAI Codex CLI",
              files_scanned: 1,
              files_skipped: [],
              candidates: [
                {
                  content: "use 4-space indent",
                  instruction: null,
                  type: "Fact",
                  priority: "Reference",
                  tags: [],
                  confidence: 0.7,
                  namespace: "global",
                  raw_excerpt: "use 4-space indent",
                  parse_confidence: "SectionSplit",
                  duplicate_of: null,
                },
                {
                  content: "guessed from an unconfirmed memory file",
                  instruction: null,
                  type: "Fact",
                  priority: "Reference",
                  tags: [],
                  confidence: 0.5,
                  namespace: "global",
                  raw_excerpt: "guessed from an unconfirmed memory file",
                  parse_confidence: "Heuristic",
                  duplicate_of: null,
                },
              ],
            },
          });
        }
        if (url.includes("/api/agents/import/run")) {
          return jsonResponse({
            ok: true,
            data: {
              agent_key: "codex",
              display_name: "OpenAI Codex CLI",
              files_scanned: 1,
              files_skipped: [],
              imported: [{ id: "mem-x", content: "use 4-space indent" }],
              duplicates_skipped: 0,
            },
          });
        }
        return null;
      }),
    );

    render(<App />);
    await screen.findByText(/No memories stored yet/);
    await act(async () => {
      screen.getByRole("button", { name: "Data" }).click();
    });

    await act(async () => {
      screen.getByRole("button", { name: "Scan for Agents" }).click();
    });
    expect(await screen.findByText("OpenAI Codex CLI")).toBeInTheDocument();
    expect(screen.getByText("Claude Code")).toBeInTheDocument();
    expect(screen.getByText(/not detected on this machine/)).toBeInTheDocument();

    await act(async () => {
      screen.getByRole("button", { name: "Preview" }).click();
    });
    expect(await screen.findByText(/use 4-space indent/)).toBeInTheDocument();
    expect(screen.getByText(/guessed from an unconfirmed memory file/)).toBeInTheDocument();
    expect(screen.getByText("⚠ heuristic")).toBeInTheDocument();

    await act(async () => {
      screen.getByRole("button", { name: "Import 2 Candidate(s)" }).click();
    });
    expect(await screen.findByText(/Imported 1 · skipped 0 duplicate/)).toBeInTheDocument();
    expect(callFor("/api/agents/import/run").method).toBe("POST");
  });
});

describe("App — memory history / restore", () => {
  it("opens the History panel from the detail view and restores a checkpoint", async () => {
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(true);
    fetchMock.mockImplementation((input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/health")) return Promise.resolve(new Response("ok", { status: 200 }));
      if (url.includes("/api/inbox")) {
        return Promise.resolve(jsonResponse({ ok: true, data: { memories: [], total: 0 } }));
      }
      if (url.includes("/api/stats")) {
        return Promise.resolve(jsonResponse({ ok: true, data: emptyStats }));
      }
      if (url.includes("/checkpoints") && url.includes("mem-h1")) {
        return Promise.resolve(
          jsonResponse({
            ok: true,
            data: [{ history_id: 7, memory_id: "mem-h1", operation: "update", changed_at: "2026-01-01T00:00:00Z" }],
          }),
        );
      }
      if (url.includes("/api/checkpoints/7/restore")) {
        return Promise.resolve(jsonResponse({ ok: true, data: { ...pendingMemory("mem-h1", "old content") } }));
      }
      if (url.includes("/api/memories")) {
        return Promise.resolve(jsonResponse({ ok: true, data: [pendingMemory("mem-h1", "current content")] }));
      }
      return Promise.resolve(jsonResponse({ ok: true, data: undefined }));
    });

    render(<App />);
    const card = await screen.findByText("current content");
    await act(async () => {
      card.click();
    });
    await act(async () => {
      screen.getByRole("button", { name: "History" }).click();
    });

    expect(await screen.findByText("update")).toBeInTheDocument();

    await act(async () => {
      screen.getByRole("button", { name: "Restore" }).click();
    });
    expect(callFor("/api/checkpoints/7/restore").method).toBe("POST");
    confirmSpy.mockRestore();
  });
});

describe("App — agents tab", () => {
  it("loads agent profiles (redacted) and per-namespace memory counts", async () => {
    fetchMock.mockImplementation((input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/health")) return Promise.resolve(new Response("ok", { status: 200 }));
      if (url.includes("/api/inbox")) {
        return Promise.resolve(jsonResponse({ ok: true, data: { memories: [], total: 0 } }));
      }
      if (url.includes("/api/agents")) {
        return Promise.resolve(
          jsonResponse({
            ok: true,
            data: [
              {
                id: "default",
                agent_type: "general-assistant",
                description: "Default agent profile",
                inject_rules: {
                  max_memories: 8,
                  token_budget: 1500,
                  priority_order: ["Must", "Reference"],
                  namespace_filter: ["global"],
                  exclude_types: [],
                },
                has_api_key: false,
              },
            ],
          }),
        );
      }
      if (url.includes("/api/stats")) {
        return Promise.resolve(
          jsonResponse({ ok: true, data: { ...emptyStats, namespaces: ["global", "project:x"] } }),
        );
      }
      if (url.includes("/api/memories?namespace=global")) {
        return Promise.resolve(jsonResponse({ ok: true, data: [pendingMemory("m1", "a"), pendingMemory("m2", "b")] }));
      }
      if (url.includes("/api/memories?namespace=project%3Ax")) {
        return Promise.resolve(jsonResponse({ ok: true, data: [pendingMemory("m3", "c")] }));
      }
      if (url.includes("/api/memories")) return Promise.resolve(jsonResponse({ ok: true, data: [] }));
      return Promise.resolve(jsonResponse({ ok: true, data: undefined }));
    });

    render(<App />);
    await screen.findByText(/No memories stored yet/);
    await act(async () => {
      screen.getByRole("button", { name: "Agents" }).click();
    });

    expect(await screen.findByText("default")).toBeInTheDocument();
    expect(screen.getByText("general-assistant")).toBeInTheDocument();
    expect(screen.getByText("Must, Reference")).toBeInTheDocument();

    expect(await screen.findByText("project:x")).toBeInTheDocument();
    const globalLabels = screen.getAllByText("global");
    expect(globalLabels.length).toBeGreaterThanOrEqual(2); // namespace_filter cell + namespace overview card
  });
});

describe("App — review tab", () => {
  it("approves a pending memory and refreshes the empty inbox", async () => {
    let approved = false;
    fetchMock.mockImplementation((input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/health")) return Promise.resolve(new Response("ok", { status: 200 }));
      if (url.includes("/api/memories")) return Promise.resolve(jsonResponse({ ok: true, data: [] }));
      if (url.includes("/api/inbox")) {
        if (url.includes("/approve")) {
          approved = true;
          return Promise.resolve(jsonResponse({ ok: true, data: true }));
        }
        return approved
          ? Promise.resolve(jsonResponse({ ok: true, data: { memories: [], total: 0 } }))
          : Promise.resolve(
              jsonResponse({ ok: true, data: { memories: [pendingMemory("mem-p1", "待评审")], total: 1 } }),
            );
      }
      if (url.includes("/api/stats")) {
        return Promise.resolve(jsonResponse({ ok: true, data: emptyStats }));
      }
      return Promise.resolve(jsonResponse({ ok: true, data: undefined }));
    });

    render(<App />);
    await screen.findByText(/No memories stored yet/);

    await act(async () => {
      screen.getByRole("button", { name: /Review \(1\)/ }).click();
    });
    expect(await screen.findByText("待评审")).toBeInTheDocument();

    await act(async () => {
      screen.getByRole("button", { name: "Approve" }).click();
    });

    await waitFor(() => {
      const approveCall = fetchMock.mock.calls.find(([u]) =>
        String(u).includes("/api/inbox/mem-p1/approve"),
      );
      expect(approveCall).toBeTruthy();
      expect((approveCall![1] as RequestInit).method).toBe("POST");
    });
    expect(await screen.findByText(/All memories have been reviewed/)).toBeInTheDocument();
  });

  it("rejects a pending memory via the dedicated inbox endpoint after the confirm dialog", async () => {
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(true);
    let rejectCalled = false;
    fetchMock.mockImplementation((input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/health")) return Promise.resolve(new Response("ok", { status: 200 }));
      if (url.includes("/api/inbox/mem-p1/reject")) {
        rejectCalled = true;
        return Promise.resolve(jsonResponse({ ok: true, data: { rejected: "mem-p1" } }));
      }
      if (url.includes("/api/memories")) return Promise.resolve(jsonResponse({ ok: true, data: [] }));
      if (url.includes("/api/inbox")) {
        return Promise.resolve(
          jsonResponse({
            ok: true,
            data: { memories: rejectCalled ? [] : [pendingMemory("mem-p1", "待删除")], total: rejectCalled ? 0 : 1 },
          }),
        );
      }
      if (url.includes("/api/stats")) {
        return Promise.resolve(jsonResponse({ ok: true, data: emptyStats }));
      }
      return Promise.resolve(jsonResponse({ ok: true, data: undefined }));
    });

    render(<App />);
    await screen.findByText(/No memories stored yet/);
    await act(async () => {
      screen.getByRole("button", { name: /Review \(1\)/ }).click();
    });
    await screen.findByText("待删除");

    await act(async () => {
      screen.getByRole("button", { name: "Reject" }).click();
    });

    await waitFor(() => {
      const rejectCall = fetchMock.mock.calls.find(([u]) =>
        String(u).includes("/api/inbox/mem-p1/reject"),
      );
      expect(rejectCall).toBeTruthy();
      expect((rejectCall![1] as RequestInit).method).toBe("POST");
    });
    expect(confirmSpy).toHaveBeenCalled();
    confirmSpy.mockRestore();
  });
});

describe("Episodic tab", () => {
  function episode(overrides: Partial<Record<string, unknown>> = {}) {
    return {
      memory_id: "mem_ep1",
      task: "deploy the dashboard",
      task_type: "deploy",
      status: "failure",
      cause: "missing env var",
      lesson: "Before 'deploy' tasks, verify: missing env var",
      lesson_memory_id: "mem_lesson1",
      occurred_at: "2026-08-26T10:00:00Z",
      ...overrides,
    };
  }

  it("shows an empty state when no outcomes are recorded", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByText(/No memories stored yet/);

    await user.click(screen.getByRole("button", { name: /Episodic/ }));
    expect(await screen.findByText(/No task outcomes recorded yet/)).toBeInTheDocument();
  });

  it("renders recorded episodes with status badges and lessons", async () => {
    fetchMock.mockImplementation((input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("/api/episodes")) {
        return Promise.resolve(
          jsonResponse({ ok: true, data: { episodes: [episode()], count: 1 } }),
        );
      }
      if (url.endsWith("/health")) {
        return Promise.resolve(new Response("ok", { status: 200 }));
      }
      if (url.includes("/api/stats")) {
        return Promise.resolve(jsonResponse({ ok: true, data: emptyStats }));
      }
      if (url.includes("/api/inbox")) {
        return Promise.resolve(jsonResponse({ ok: true, data: { memories: [], total: 0 } }));
      }
      return Promise.resolve(jsonResponse({ ok: true, data: [] }));
    });

    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByRole("button", { name: /Episodic/ }));

    expect(await screen.findByText("deploy the dashboard")).toBeInTheDocument();
    expect(screen.getByText("missing env var")).toBeInTheDocument();
    expect(screen.getByText(/verify: missing env var/)).toBeInTheDocument();
    const badge = document.querySelector(".status-badge.status-failure");
    expect(badge).not.toBeNull();
    expect(badge!.textContent).toBe("failure");
  });

  it("submits the outcome form via POST /api/outcome", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByText(/No memories stored yet/);
    await user.click(screen.getByRole("button", { name: /Episodic/ }));
    await screen.findByText(/No task outcomes recorded yet/);

    const [taskInput] = screen.getAllByPlaceholderText("deploy the dashboard");
    await user.type(taskInput, "build the image");
    const causeInput = screen.getByPlaceholderText("missing env var");
    await user.type(causeInput, "registry timeout");

    await user.click(screen.getByRole("button", { name: "Record Outcome" }));

    await waitFor(() => {
      const call = fetchMock.mock.calls.find(
        ([input, init]) =>
          String(input).endsWith("/api/outcome") &&
          (init as RequestInit)?.method === "POST",
      );
      expect(call).toBeTruthy();
      const body = JSON.parse((call![1] as RequestInit).body as string);
      expect(body.task).toBe("build the image");
      expect(body.status).toBe("success");
      expect(body.cause).toBe("registry timeout");
      expect(body.agent_id).toBe("dashboard");
    });
  });
});
