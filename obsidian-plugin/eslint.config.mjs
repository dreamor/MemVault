import obsidian from 'eslint-plugin-obsidianmd';
import tseslint from 'typescript-eslint';

// Mirrors the community directory automated review so findings can be
// reproduced (and cleared) locally *before* a version is released. The review
// runs this against the published source, so anything it can flag must fail
// here first — v0.3.5 shipped an `obsidianmd/no-unsupported-api` error because
// nothing in CI ran the linter.
//
// This file lives in the monorepo (the source of truth for src/) and is synced
// to dreamor/memvault-obsidian by scripts/sync-obsidian-plugin.sh.
export default tseslint.config(
  tseslint.configs.recommendedTypeChecked,
  obsidian.configs.recommended,
  {
    // Tests are not shipped and are excluded from tsconfig (so the type-aware
    // lint rules cannot build a program for them); the review only sees the
    // plugin source.
    ignores: ['src/**/*.test.ts', 'out/**'],
  },
  {
    // Brand names ("MemVault") and ALL-CAPS priorities ("MUST") are legitimate
    // UI text; the rule keeps flagging them. The community directory review
    // does not gate on this rule.
    rules: { 'obsidianmd/ui/sentence-case': 'off' },
  },
  {
    languageOptions: {
      parserOptions: {
        projectService: true,
        tsconfigRootDir: import.meta.dirname,
      },
    },
  },
);
