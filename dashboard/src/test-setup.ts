import "@testing-library/jest-dom/vitest";
import { Blob as NodeBlob } from "node:buffer";
import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";

// The jsdom environment swaps the global Blob for its own implementation,
// which lacks .stream(). Node's native fetch Response constructor (undici)
// calls body.stream() when handed a Blob, so Response(new Blob(...)) test
// fixtures blew up with "object.stream is not a function". Restore Node's
// native Blob globally — it's a superset of what the tests and app code use
// (size/type/slice/arrayBuffer/text/stream all supported).
(globalThis as any).Blob = NodeBlob;

// jsdom (vitest environment: jsdom) may not expose localStorage depending on
// the Node version / vitest config. Provide a minimal in-memory shim.
const store = new Map<string, string>();
(globalThis as any).localStorage ??= {
  getItem: (k: string) => store.get(k) ?? null,
  setItem: (k: string, v: string) => void store.set(k, String(v)),
  removeItem: (k: string) => void store.delete(k),
  clear: () => store.clear(),
};

afterEach(() => {
  cleanup();
});
