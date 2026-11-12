import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import type { RemoteMemory } from './sync';

/**
 * The consumer half of the wire contract in `contracts/memory-json-keys.json`.
 *
 * The producer half is `test_memory_json_matches_the_wire_contract` in
 * crates/memvault-mcp/src/rest_api.rs, which asserts the same manifest against
 * `memory_to_json`. Two tests and one manifest, because the two toolchains
 * cannot see each other's types — the file is the only thing they share.
 *
 * Without this, `RemoteMemory` was a hand-kept copy of the REST payload that
 * had drifted to 10 of 24 fields with nothing watching it. Every change up to
 * that point had been additive so nothing broke; a rename would have arrived as
 * `undefined` and been written into note frontmatter as an empty value.
 */

const MANIFEST_PATH = fileURLToPath(
  new URL('../../contracts/memory-json-keys.json', import.meta.url),
);

interface Manifest {
  keys: string[];
  renames: Record<string, string>;
  omitted_from_wire: string[];
}

function loadManifest(): Manifest {
  return JSON.parse(readFileSync(MANIFEST_PATH, 'utf8')) as Manifest;
}

/**
 * Every field of `RemoteMemory`, populated.
 *
 * This object *is* the key set under test, so it has to stay complete. It is
 * typed `Required<RemoteMemory>` and `tsconfig.json` includes test files in
 * `typecheck` so that even optional legacy-compatible fields cannot disappear
 * here — or appear here but not in the interface — without a compile error.
 * Values are arbitrary; only the keys are load-bearing.
 */
const FIXTURE: Required<RemoteMemory> = {
  id: 'mem_contract0',
  content: 'wire contract fixture',
  instruction: 'prefer the explicit form',
  priority: 'MUST',
  memory_type: 'skill',
  namespace: 'project:contract',
  tags: ['contract'],
  layer: 'L3',
  human_reviewed: true,
  ai_generated: true,
  identity_verified: true,
  confidence: 0.75,
  access_count: 3,
  decay_score: 0.25,
  created_at: '2026-08-14T09:00:00Z',
  updated_at: '2026-08-14T09:00:00Z',
  occurred_at: '2026-08-14T09:00:00Z',
  expires_at: '2026-09-14T09:00:00Z',
  archived_at: '2026-08-20T09:00:00Z',
  source_agent: 'contract-test',
  visibility: 'shared',
  superseded_by: 'mem_successor',
  proposes_supersede_of: 'mem_target',
  skill_meta: {
    trigger: 'on deploy',
    steps: ['build', 'ship'],
    verification: 'smoke test passes',
    version: 2,
  },
  friction_evidence: 'user pushed back twice',
  fact_assertion: {
    key: 'deploy.tool',
    value: 'explicit',
    source_namespace: 'project:contract',
    source_order: 2,
    cardinality: 'single',
  },
  fact_management: {
    record_id: 'mem_contract0',
    producer_id: 'contract-test',
    namespace: 'project:contract',
    source_namespace: 'project:contract',
    key: 'deploy.tool',
    source_order: 2,
    state: 'active',
  },
};

/** Translate a `RemoteMemory` key back to the name it travels under. */
function toWireName(pluginKey: string, renames: Record<string, string>): string {
  for (const [wire, plugin] of Object.entries(renames)) {
    if (plugin === pluginKey) return wire;
  }
  return pluginKey;
}

describe('REST wire contract', () => {
  it('reads the manifest both toolchains share', () => {
    const manifest = loadManifest();
    expect(manifest.keys.length).toBeGreaterThan(0);
    // Resolved relative to this file, not to the process cwd — vitest is run
    // from obsidian-plugin/ by npm and from the repo root by CI's matrix.
    expect(MANIFEST_PATH).toContain('contracts/memory-json-keys.json');
  });

  it('RemoteMemory declares exactly the contracted wire keys', () => {
    const manifest = loadManifest();
    const actual = Object.keys(FIXTURE)
      .map((k) => toWireName(k, manifest.renames))
      .sort();
    expect(actual).toEqual([...manifest.keys].sort());
  });

  it('the manifest is canonical: sorted and duplicate-free', () => {
    // The Rust test asserts the same, but each side runs in a CI job that
    // cannot see the other, and an unsorted manifest makes every future diff
    // against it noisy enough to hide a real change.
    const manifest = loadManifest();
    for (const [name, list] of [
      ['keys', manifest.keys],
      ['omitted_from_wire', manifest.omitted_from_wire],
    ] as const) {
      expect(list, `${name} must be sorted`).toEqual([...list].sort());
      expect(new Set(list).size, `${name} must not repeat`).toBe(list.length);
    }
    expect(
      manifest.keys.filter((k) => manifest.omitted_from_wire.includes(k)),
      'a field cannot be both exposed and omitted',
    ).toEqual([]);
  });

  it('the only rename is type -> memory_type', () => {
    // Pinned because `folderFor` and the note body both read `memory_type`,
    // and main.ts's listMemories adapter is the single place that performs the
    // translation. A second rename appearing here means a second adapter is
    // needed somewhere and the plugin will read `undefined` until it exists.
    expect(loadManifest().renames).toEqual({ type: 'memory_type' });
  });

  it('RemoteMemory does not declare fields the server omits', () => {
    const manifest = loadManifest();
    const wireNames = Object.keys(FIXTURE).map((k) => toWireName(k, manifest.renames));
    expect(
      wireNames.filter((k) => manifest.omitted_from_wire.includes(k)),
      'these Memory fields are deliberately not on the wire; declaring them ' +
        'here would give every consumer a permanently undefined field',
    ).toEqual([]);
  });
});
