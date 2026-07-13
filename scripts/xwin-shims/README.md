# Windows cross-check from macOS

Type-check the Windows build (incl. teil-capture-windows) without a Windows machine:

```sh
cargo install cargo-xwin                      # once
rustup target add x86_64-pc-windows-msvc      # once
cd src-tauri
PATH="$PWD/../scripts/xwin-shims:$PATH" cargo xwin check --target x86_64-pc-windows-msvc
```

cargo-xwin provides the MSVC/SDK *headers*; the two shims here stand in for
`llvm-lib` (ring's static-lib archiver) and `llvm-rc` (tauri-winres resource
compiler), whose outputs are only consumed at link time — which `cargo check`
never reaches. This is check-only: real Windows binaries must be built on Windows.
