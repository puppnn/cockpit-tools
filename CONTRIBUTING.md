# Contributing to Cockpit Tools

Thank you for your interest in contributing to Cockpit Tools! This project aims to be the universal manager for AI IDEs, and we welcome contributions of all kinds.

## 🚀 Getting Started

1.  **Fork** the repository on GitHub.
2.  **Clone** your fork locally:
    ```bash
    git clone https://github.com/YOUR_USERNAME/cockpit-tools.git
    ```
3.  **Create a branch** for your feature or bug fix:
    ```bash
    git checkout -b feature/my-cool-feature
    ```

## 🛠️ Project Structure

This project is a Cargo Workspace:
- `crates/cockpit-core`: Shared business logic (Library).
- `src-tauri`: The GUI application (Tauri + React).
- `crates/cockpit-cli`: The command-line interface.

## 📝 Coding Standards

- **Rust:** Follow standard Rust idioms. Run `cargo fmt` before committing.
- **Frontend:** We use React 19 and Tailwind CSS. Use functional components and hooks.
- **Commits:** Use clear, descriptive commit messages.

## 🧪 Testing

- **GUI:** `npm run tauri dev`
- **CLI:** `cargo run --package cockpit-cli -- <commands>`
- **Core:** `cargo test --package cockpit-core`

## 📬 Submitting a Pull Request

1.  Push your changes to your fork.
2.  Open a Pull Request against the `main` branch.
3.  Provide a clear description of the changes and link any related issues.
4.  Be prepared to iterate based on feedback!

## 📜 Code of Conduct

Please be respectful and professional in all interactions. We follow the [Contributor Covenant](https://www.contributor-covenant.org/).
