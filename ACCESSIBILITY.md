# Accessibility

MemVault is a local-first memory layer for AI agents. Most of it is
infrastructure — a Rust/SQLite core, an MCP/REST server, and CLI — but it also
ships interfaces people read and operate directly: the Web Dashboard, the CLI
output, the documentation, and a set of client plugins that render inside host
apps. This statement describes our commitment to those interfaces, the
environments we support, what we know is not yet accessible, and how to report a
barrier.

Accessibility is a **best-effort, ongoing commitment**, not a certification.
We have not commissioned a formal WCAG conformance audit, and we would rather
state our gaps honestly than imply a level of assurance we have not earned.

## Scope: where accessibility applies

| Surface | What it is | Who owns the accessibility of it |
|---------|------------|----------------------------------|
| **Web Dashboard** (`dashboard/`) | React + Vite + TypeScript UI for browsing, reviewing, and managing memories | MemVault (this statement) |
| **CLI** (`memvault-cli`) | Terminal commands and plain-text output | MemVault (this statement) |
| **MCP / REST server** (`memvault-mcp`) | Machine protocol, no human UI | N/A — consumed by agents, not people |
| **Client plugins** (Obsidian, DSH, Codex, Claude Code, …) | Settings panes and injected text rendered inside a host app | Mostly the host app; MemVault follows the host's component patterns |
| **Documentation** (`README.md`, `README.zh-CN.md`, `docs/`) | Markdown rendered by GitHub or the reader's tool | MemVault (this statement) |
| **Brand assets** (`assets/`) | Logo and images used in docs and the dashboard | MemVault (this statement) |

## Supported environments

**Web Dashboard**

- Modern evergreen browsers: Chrome, Edge, Firefox, Safari (desktop and mobile
  viewports). Only a responsive layout and standard DOM/CSS are used.
- Keyboard, browser zoom, and system font scaling are supported.
- Two themes (dark, light) and two UI locales (English, Simplified Chinese),
  both persisted and applied before first paint so there is no flash of the
  wrong theme or language.
- Respects the operating system `prefers-reduced-motion` setting — animations
  are reduced to near-zero rather than removed mid-interaction.

**CLI**

- macOS, Linux, and Windows (PowerShell / Windows Terminal). Output is plain
  UTF-8 text with no ANSI color escapes, so it works with screen readers and
  screen magnifiers that read a terminal buffer (VoiceOver in Terminal, NVDA or
  Narrator in Windows Terminal, Orca on Linux).
- Long output relies on your terminal's own scrolling and paging.

**Client plugins**

- Accessibility of the plugin UI is largely inherited from the host application
  (Obsidian, Codex, Claude Code, dsh, …). Where a plugin contributes its own
  settings, it uses the host's standard `Setting`/component primitives so the
  host's focus, contrast, and screen-reader behavior apply.

## What we have implemented

- **Dashboard landmarks and semantics**: a `<main role="main">` landmark, real
  `<table>` markup for tabular data, and heading hierarchy.
- **Icon-only controls are named**: theme toggle, language toggle, close
  buttons, search-run buttons, and filters carry `aria-label` (localized), so
  they are announced correctly instead of as an unlabeled glyph.
- **Decorative graphics are hidden**: non-meaningful SVGs use `aria-hidden`
  and every meaningful image has descriptive `alt` text (for example, the
  README logo).
- **State and errors are announced**: error messages use `role="alert"`;
  the active tab is exposed with `aria-current="page"`; dialogs use
  `role="dialog"` with `aria-modal="true"`.
- **Keyboard-operable custom controls**: clickable table rows are focusable
  (`tabIndex={0}`) and respond to both `Enter` and `Space`, not mouse clicks
  alone.
- **Reduced motion**: `@media (prefers-reduced-motion: reduce)` is honored
  dashboard-wide.
- **Language and theme**: the document `lang` attribute tracks the selected
  locale, and `color-scheme` / `theme-color` are set for the OS and browser.

## Known limitations

We track these openly. None of them mean "won't fix" — they are the current
state, and issues and pull requests that close them are welcome.

- **No formal audit.** MemVault has not been tested against WCAG 2.1/2.2 AA by
  an independent auditor, and we do not claim a conformance level.
- **No end-to-end screen-reader verification of the Dashboard yet.** The
  semantics above are implemented, but we have not run a full keyboard-only and
  screen-reader pass across every tab and task.
- **The Dashboard tab bar is not a full ARIA tab pattern.** Tabs are buttons
  reachable with `Tab` and marked with `aria-current`, but there is no
  `role="tablist"`/`role="tab"` wiring and no arrow-key roving focus.
- **Modal dialogs do not yet manage focus.** `role="dialog" aria-modal="true"`
  is set, but focus is not trapped inside an open dialog, is not returned to the
  trigger on close, and `Escape` does not dismiss it — you close via the ×
  button or by clicking outside.
- **Color alone sometimes carries meaning** in statistics and status
  indicators, and there is no dedicated high-contrast theme. The dark and light
  palettes have not been formally checked against WCAG AA contrast ratios.
- **No motion or sound alternatives for data visuals.** Charts and animated
  status indicators have no non-visual equivalent.
- **Localization is partial.** The Dashboard ships English and Simplified
  Chinese; the documentation is English-first (`README.zh-CN.md` is the
  translated entry point). Other locales fall back to English.
- **Research artifacts are best-effort.** Files such as `docs/experiments/`
  and `design-system/` are generated or exploratory and may not meet the
  documentation standards above.

## Reporting an accessibility barrier

If any part of MemVault — the Dashboard, CLI, docs, or a plugin — is difficult
or impossible for you to use, please tell us. Concrete reports are what move
these issues up the list.

- **Preferred**: open a GitHub issue at
  <https://github.com/dreamor/memvault/issues/new>. If you can, prefix the title
  with `Accessibility:` (for example, `Accessibility: theme toggle not
  reachable by keyboard`).
- **Discussion**: for open-ended questions or ideas, use
  <https://github.com/dreamor/memvault/discussions>.
- **Security-related barriers** (for example, an assistive-technology path that
  could leak data) go through the private channel in
  [SECURITY.md](SECURITY.md), **not** a public issue.

Helpful details in a report: which surface and version, your OS, browser or
terminal, assistive technology and version, what you were trying to do, what
happened, and what you expected. A screenshot or recording is welcome but never
required.

We commit to acknowledging accessibility reports within **7 days** and to
keeping the issue open with a status note until it is resolved or explicitly
deprioritized with a reason.

## Accessibility for contributors

If you contribute to a user-facing surface, please:

- Keep the semantics already in place — landmarks, `aria-label` on icon-only
  controls, `aria-hidden` on decorative graphics, `alt` text on images.
- Make anything clickable also keyboard-operable (`Enter`/`Space`) and
  focus-visible.
- Respect `prefers-reduced-motion`; do not add motion that cannot be reduced.
- Do not encode meaning in color alone.
- Include accessibility considerations in the "Testing Notes" of your pull
  request, alongside the screenshot the template already asks for on UI
  changes. Dashboard tests live next to the source (`dashboard/src/*.test.tsx`).

## References

- [Open Source Guide — Accessibility best practices for your project](https://opensource.guide/accessibility-best-practices-for-your-project/)
- [accessibility-toolkit ACCESSIBILITY.md example](https://github.com/open-source-accessibility/accessibility-toolkit/blob/main/ACCESSIBILITY.md)
- [WCAG 2.2](https://www.w3.org/TR/WCAG22/)
