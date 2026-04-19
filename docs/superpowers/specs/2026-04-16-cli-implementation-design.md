# Design Spec: CLI Implementation for Cockpit Tools

**Date:** 2026-04-16
**Status:** Draft
**Author:** Gemini CLI & User

## 1. Overview
The goal is to extend Cockpit Tools from a GUI-only application into a multi-interface tool by adding a Command Line Interface (CLI). The CLI will allow for headless management, scripting, and faster workflows for power users, while sharing 100% of the logic and state with the existing GUI application.

## 2. Architecture
We will migrate the project to a **Cargo Workspace** to enable code sharing between the Tauri GUI and the new CLI.

### 2.1 Workspace Structure
- **`crates/cockpit-core` (Library):** Contains all business logic, including account management, platform-specific injectors, process management, and configuration handling.
- **`src-tauri` (Tauri GUI):** A consumer of `cockpit-core`. It handles the desktop window, system tray, and frontend-backend bridge.
- **`crates/cockpit-cli` (Binary):** A consumer of `cockpit-core`. It provides the terminal interface using `clap`.

### 2.2 Shared State
Both interfaces will interact with the same data directory: `~/.antigravity_cockpit`. This ensures that an account switch in the CLI is immediately reflected in the GUI and vice versa.

## 3. CLI Design
The CLI will be named `cockpit`.

### 3.1 Command Hierarchy
- `cockpit list [platform]`
  - Lists accounts. If platform is omitted, shows all.
- `cockpit switch <platform> <account_id>`
  - Sets the active account for a specific platform by injecting credentials into the IDE's local configuration.
- `cockpit launch <platform> [--instance <id>]`
  - Launches the IDE instance.
- `cockpit quota [platform]`
  - Displays remaining usage for the specified platform.
- `cockpit config <key> [value]`
  - Gets or sets configuration values (e.g., app paths).

### 3.2 User Experience
- **Output:** Human-readable tables by default; `--json` flag for scripting.
- **Errors:** Clear, actionable error messages with exit codes.
- **Auto-detection:** The CLI will automatically locate the data directory and binaries based on existing `config.json` rules.

## 4. Migration Strategy
To maintain stability, the migration will follow these steps:
1. **Scaffold Workspace:** Create the root `Cargo.toml` and move core utilities to `cockpit-core`.
2. **Module Migration:** Move modules (Cursor, Gemini, etc.) one by one from `src-tauri/src/modules` to `crates/cockpit-core/src`.
3. **GUI Update:** Update Tauri commands to call the new library functions.
4. **CLI Implementation:** Incrementally add commands to `cockpit-cli` as platforms are moved to `core`.

## 5. Success Criteria
- [ ] A standalone `cockpit` binary is produced.
- [ ] Switching an account via CLI updates the same files used by the GUI.
- [ ] No regression in GUI functionality.
- [ ] CLI supports at least: Cursor, Gemini, and GitHub Copilot in the first release.
