# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Current state

This is a greenfield prototype. **There is no application code yet** — no Rust crates, no `Cargo.toml`, no C# sources. What exists is scaffolding:

- `DemoRustMonoMicroservice.slnx` — Visual Studio solution (XML `.slnx` format, not `.sln`). Holds a "Solution Items" folder for the root-level files plus the `Microservices` project.
- `Microservices/Microservices.shproj` + `.projitems` — an **empty C# shared project** (`HasSharedItems`, root namespace `Microservices`). A shared project contributes no build output of its own; its `.projitems` `<ItemGroup>` is currently empty, and referencing projects would compile its files into themselves.
- `hrdrClaudeNative.cmd`, `RunClaude.cmd` — agent launcher scripts (see below).

## Where Rust code goes

**All Rust microservices live under `Microservices/`** — crates in there, never sibling top-level directories.

**Every `.rs` file must be registered in `Microservices/Microservices.projitems`** as an inert `<None Include="…" />` item. A shared project only surfaces files listed in `.projitems`, so an unregistered file is invisible in Solution Explorer. Adding or removing a Rust file is a two-step operation: change the file on disk *and* update `.projitems` in the same change. `<None>` items are never fed to the C# compiler, so this is display-only and cannot break a build.

Visual Studio is a **viewer** for this code, not its build system — the `.shproj` produces no build output and the C# code-sharing targets do nothing with `.rs`. Build, test, and lint with cargo from the command line; don't route those through Visual Studio. Do not restructure or delete `.shproj` / `.projitems` for tidiness — they are the only thing making the folder openable in VS.

## Agent launchers

`RunClaude.cmd` is the single place where the Claude Code invocation lives:

```
claude --dangerously-skip-permissions --verbose [--model %CLAUDE_MODEL%]
```

It `pushd`es to the repo root first, so it works from any cwd. Changes to Claude's launch flags belong here, not duplicated elsewhere.

`hrdrClaudeNative.cmd` wraps `RunClaude.cmd` to launch Claude as a tracked, named pane inside the `herdr` terminal multiplexer. It is self-healing: locates `herdr` on PATH or at `%LOCALAPPDATA%\Programs\Herdr\bin\herdr.exe`, installs/updates herdr's native Claude integration hook, starts a herdr server if none is running, creates (or reuses) a workspace labelled after the repo directory, picks a free agent name (`claude`, `claude-2`, …), runs `RunClaude.cmd` in the new pane, then renames the detected agent and attaches this console.

Model selection flows `hrdrClaudeNative.cmd` → pane env (`CLAUDE_MODEL`) → `RunClaude.cmd` → `--model`. Default is `claude-opus-5`; override with `setx CLAUDE_MODEL "claude-sonnet-5"`.

When editing these scripts, note the constraints already worked around: herdr agent names must match `[a-z][a-z0-9_-]{0,31}` and be unique among live agents; herdr replies in JSON so lookups are delegated to an inline `powershell -NoProfile` call that reads `HERDR`/`REPO_DIR`/`WS_LABEL` from the environment to avoid nested quoting; `pane run` commands are wrapped in `cmd /c` because panes may run PowerShell.

## Repo conventions

- `.gitignore` is the standard GitHub VisualStudio template — it does **not** cover Rust (`target/`) directory. `data/cipher-sessions.db*` is runtime state from the Cipher MCP server and should not be committed; extend `.gitignore` when adding Rust or keeping local data.
- `.gitattributes` sets `* text=auto`; all the merge-driver and diff sections are commented out.
