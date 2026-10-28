#!/bin/sh
# Formatting, Clippy and tests for the engine and its binding. Needs the
# windowing-gos checkout beside this one (rust/.cargo/config.toml) and no
# display. Run from each crate so its .cargo configuration applies.
set -eu
RUSTUP_TOOLCHAIN=${RUSTUP_TOOLCHAIN:-1.95.0}
export RUSTUP_TOOLCHAIN
root=$(cd "$(dirname "$0")/.." && pwd)
# The binding SDK is not on crates.io. Take it where the projects gos
# generates do: GOSSAMER_BINDING_PATH when set, else what `gos env` reports,
# the checkout gos was built from or its release tag (URL@TAG).
sdk=${GOSSAMER_BINDING_PATH:-$("${GOS:-gos}" env | awk '$1 == "binding_sdk" { print $2 }')}
case "$sdk" in
    https://*@*)
        sdk_source="patch.crates-io.gossamer-binding.git=\"${sdk%@*}\""
        sdk_ref="patch.crates-io.gossamer-binding.tag=\"${sdk##*@}\""
        ;;
    *)
        sdk_source="patch.crates-io.gossamer-binding.path=\"$sdk\""
        sdk_ref=
        ;;
esac
for crate in rust/gui-engine rust/binding; do
    (
        cd "$root/$crate"
        cargo fmt --all --check
        # Only the binding depends on the SDK. Applying this patch to the core
        # adds an unused-patch lock entry.
        set --
        if [ "$crate" = rust/binding ]; then
            set -- --config "$sdk_source"
            [ -z "$sdk_ref" ] || set -- "$@" --config "$sdk_ref"
        fi
        cargo clippy "$@" --all-targets -- -D warnings
        cargo test "$@" --all-targets
    )
done
