# rust-lower

Nightly-pinned MIR lowerer: emits vikt `FunctionIr` JSON via `rustc_private`
(`rustc_public`, née `stable_mir`) from the toolchain's `rustc-dev` component.
Deliberately outside the stable workspace; see `rust-toolchain.toml`.

## Watch: `rustc_public` crates.io publication

Not yet published (checked 2026-08-17: sparse index 404s for both
`rustc_public` and `stable_mir`). Migration should be nearly mechanical —
this crate already uses only the public `rustc_public` API surface, so
publishing mainly swaps the `rustc-dev` sysroot link + nightly pin for a
plain dependency; no call-site rewrites expected. Track:
https://github.com/rust-lang/rust-project-goals/issues/266
