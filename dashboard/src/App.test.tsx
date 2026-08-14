import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "./App";

const invokeMock = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

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

function mockInvokeDefaults() {
  invokeMock.mockImplementation((cmd: string) => {
    switch (cmd) {
      case "list_memories":
        return Promise.resolve([]);
      case "get_stats":
        return Promise.resolve(emptyStats);
      case "get_db_path":
        return Promise.resolve("/home/test/.memvault/data.db");
      case "get_compliance_summary":
        return Promise.reject(new Error("Compliance tracking is not enabled"));
      default:
        return Promise.resolve(undefined);
    }
  });
}

beforeEach(() => {
  invokeMock.mockReset();
  mockInvokeDefaults();
});

describe("App", () => {
  it("renders the Memories tab with an empty state by default", async () => {
    render(<App />);
    expect(await screen.findByText(/No memories stored yet/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Memories/ })).toHaveClass("active");
  });

  it("switches to the Settings tab and shows the active DB path", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByText(/No memories stored yet/);

    await user.click(screen.getByRole("button", { name: "Settings" }));
    expect(await screen.findByText("/home/test/.memvault/data.db")).toBeInTheDocument();
  });

  it("submits the New Memory form via create_memory", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      switch (cmd) {
        case "list_memories":
          return Promise.resolve([]);
        case "get_stats":
          return Promise.resolve(emptyStats);
        case "create_memory":
          return Promise.resolve({
            id: "mem_new1",
            memory_type: "Fact",
            content: "test memory",
            instruction: null,
            priority: "Reference",
            namespace: "global",
            tags: [],
            source_agent_id: "dashboard",
            confidence: 0.8,
            human_reviewed: true,
            decay_score: 1.0,
            access_count: 0,
            layer: "L2",
            skill_meta: null,
            created_at: "2026-08-14T00:00:00Z",
            updated_at: "2026-08-14T00:00:00Z",
          });
        default:
          return Promise.resolve(undefined);
      }
    });

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
      expect(invokeMock).toHaveBeenCalledWith(
        "create_memory",
        expect.objectContaining({
          req: expect.objectContaining({ content: "test memory" }),
        }),
      );
    });
  });
});
