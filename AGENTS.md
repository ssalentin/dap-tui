# dap-tui — agent notes

- Run `cargo test` after any change (parser, engine, and TestBackend UI tests live in-tree).
- **After every code change, reinstall the binary** so `dap-tui` on PATH picks it up:
  ```bash
  cargo install --path . --force
  ```
  Verify the install with:
  ```bash
  sha256sum ~/.cargo/bin/dap-tui target/release/dap-tui   # hashes must match
  ```
- Commit and push changes to the Gitea remote (`origin`) when done.

See `../AGENTS.md` for the global rules (sudo/pkexec, Gitea push, etc.).
