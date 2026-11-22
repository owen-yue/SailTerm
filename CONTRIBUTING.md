# Contributing to SailTerm

Thank you for your interest in improving SailTerm! This guide covers how to set
up a development environment, the conventions the codebase follows, and the
checks to run before opening a pull request.

> **A note on language.** SailTerm targets Chinese server-operations users
> first, so many **source comments** and the **AI tool prompts** (tool
> `description()` / JSON schemas in `src-tauri/src/ai/tools.rs`) are written in
> Chinese on purpose — they are functional instructions to the LLM, not UI text.
> The user interface itself is fully internationalized. When you touch a file,
> translating nearby Chinese comments to English in the same commit is welcome,
> but please do not mass-rewrite comments in files you are not otherwise
> changing.

## Development setup

Prerequisites (see also the [README](README.md#requirements)):

- **Node.js** 20+ with **pnpm**
- **Rust** stable toolchain
- Tauri 2 platform dependencies (WebView2 + C++ Build Tools on Windows, Xcode
  CLT on macOS, `webkit2gtk` etc. on Linux)

```bash
pnpm install        # install frontend dependencies
pnpm tauri dev      # run the full desktop app with hot reload
```

## Project layout

```
src/                # React + TypeScript frontend (view zone — no credentials, no network)
  components/       # UI, organized feature-first (ai / terminal / file / settings / ...)
  lib/              # utilities: i18n, themes, agent mode, shell guards, Tauri bindings
  locales/          # built-in UI strings: en.ts / zh.ts
  stores/           # state management
src-tauri/          # Rust core (trusted zone — owns credentials and real sessions)
  src/
    ai/             # orchestration, tools, policy engine, sanitizer, approval
    commands/       # Tauri commands exposed to the frontend
    files/          # unified SFTP/FTP backend
    session/        # session manager & capabilities
examples/           # example plugins
scripts/            # dev / lint / packaging scripts
```

## Architecture rules to respect

SailTerm's security model is **three-zone isolation** (see
[SECURITY.md](SECURITY.md)). A few invariants must hold for any change:

- **The frontend never touches credentials or makes network requests.** All
  privileged work happens in the Rust core; the WebView only renders and
  interacts.
- **AI tool arguments carry business parameters only — never credentials.**
  `session_id` is bound by the orchestrator and is not exposed in the LLM-visible
  schema.
- **Risk decisions (policy tiering / approval / sanitization) happen in
  non-bypassable orchestrator nodes**, not inside individual tools. Tool
  `invoke()` returns raw, un-sanitized output that must not go out to the
  network directly.

## Code conventions

**Frontend (React + TypeScript)**

- Prefer the shared UI primitives instead of one-off markup: the common
  `<Button>` component, the custom `Select`, and the global `Toasts` notification
  layer for user-facing messages. Keep visual styles in the global CSS
  convergence layer rather than inline.
- **All user-facing UI strings go through the i18n system** (`src/lib/i18n.ts`
  with dictionaries in `src/locales/en.ts` and `src/locales/zh.ts`). When you add
  a string, add **both** the `en` and `zh` entries — do not ship a key in only one
  language.

**Rust (core)**

- Keep the IPC surface minimal and explicit; `scripts/check-ipc-boundary.sh`
  lints the frontend/backend command boundary as part of `pnpm check`.
- Reuse existing services and backends (e.g. the unified file/transfer backend)
  rather than reimplementing an operation that already exists elsewhere.

## Before opening a pull request

Run the checks locally and make sure they pass:

```bash
pnpm check          # tsc --noEmit + IPC boundary lint + vite build
cd src-tauri
cargo check         # backend type/check
cargo test          # backend unit tests
```

Keep commits focused. Write a clear PR description that explains the *why*, and
link any related issue.

## Reporting security issues

Please do **not** open a public issue for vulnerabilities. See
[SECURITY.md](SECURITY.md) for the disclosure process.

## License

By contributing, you agree that your contributions are licensed under the
project's [MIT License](LICENSE).
