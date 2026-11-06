# Dapital patches for gpui-pre 0.3.11

This branch holds the crates.io sources of the `gpui-pre*` crates that
Dapital patches, one directory per crate. `gpui-pre 0.3.11` is a snapshot of
zed-industries/zed `f816e1f9263e3a8ef63c4baaa02e1441ce1c6038`.

- The first commit holds the published sources, unmodified. `SOURCES` lists
  each crate's version and the SHA-256 of its `.crate` file.
- Each later commit is one patch.
- Meatspace-Industries/juggernaut `desktop/rust/Cargo.toml` applies this
  branch with `[patch.crates-io]`, pinned by `rev`.

GitHub Actions is disabled on this fork.
