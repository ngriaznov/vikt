# Working on vikt

## Before any commit or push — every clone, every session

1. Activate the versioned git hooks FIRST, before creating any commit:

       git config core.hooksPath .githooks

   The `commit-msg` hook strips AI attribution trailers (`Co-Authored-By:
   ... Claude ...`, `Claude-Session:`) from commit messages. Committing
   without it reintroduces a Claude contributor entry, which this
   repository does not want.

2. Author commits as the repository owner, not as Claude:

       git config user.name "ngriaznov"
       git config user.email "17167893+ngriaznov@users.noreply.github.com"

If a commit was created before the hooks were active, amend it to strip
the trailers before pushing.

## Gate before any push

    cargo fmt && cargo build --release -q && cargo test --release -q \
      && cargo clippy --release -q --all-targets -- -D warnings

All four must pass. The Rust-frontend integration tests skip unless the
nightly MIR helper is built (`cd tools/rust-lower && cargo build --release`,
pinned toolchain fetched automatically).
