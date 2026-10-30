# gui-engine-gos

The engine of the Gossamer desktop toolkit, written in Rust: widgets, layout,
text shaping, software painting, themes, dialogs, accessibility and the
application loop, plus the terminal, docking, printing, chart, scene, image
and media modules. It runs on [windowing-gos](https://github.com/gossamer-lang/windowing-gos).

Applications do not use this package directly. They use
[gui-gos](https://github.com/gossamer-lang/gui-gos) (`use gui`), the Gossamer library built on it.

## Layout

| Path | Holds |
|---|---|
| `rust/gui-engine` | The `gui-engine` crate: the engine itself. Rust programs can drive it directly through `Session`, `App`, `application` and `launch`. |
| `rust/binding` | The `gui_engine_host` binding module that gui-gos calls: sessions by id, widget trees, messages and queries as JSON, and Gossamer closures as callback ids. |
| `src/lib.gos` | `use gui_engine`: Gossamer functions over the binding module. |

Gossamer closures the toolkit calls back (a view, an update, a validator, a
custom widget's drawing) are registered with `gui_engine::register` and
passed as ids; `gui_engine::release` lets one go. Callbacks run on the UI
thread.

## Documentation

The toolkit is documented with gui-gos:

- [Architecture](https://gossamer-lang.org/docs/ecosystem/gui/architecture/)
- [Rendering](https://gossamer-lang.org/docs/ecosystem/gui/rendering/)
- [Resource limits](https://gossamer-lang.org/docs/ecosystem/gui/resources/)
- [Documents and editing](https://gossamer-lang.org/docs/ecosystem/gui/documents/)
- [Modules](https://gossamer-lang.org/docs/ecosystem/gui/modules/)

## Requirements

Gossamer 0.75.8 or newer and a Rust toolchain. windowing-gos, which brings
SDL 3, comes in as a dependency at its v0.1.0 release, which `gos fetch`
prepares. On Linux, D-Bus (accessibility, notifications, single instance) is
spoken directly, so libdbus is not needed.

## Testing

```sh
cd rust/gui-engine
cargo test
cargo clippy --all-targets -- -D warnings
```

Tests run one at a time, because SDL video calls must come from the main
thread, and use SDL's offscreen video driver, so they need no display.

The engine asks for `gos-windowing` by version. A program's build links the
copy windowing-gos carries, which `gos` patches in; Cargo runs in the crates
under `rust/` take a windowing-gos checkout beside this one the same way
(`rust/.cargo/config.toml`).

The binding crate needs the Gossamer binding SDK, which is not published on
crates.io. Pass
`--config 'patch.crates-io.gossamer-binding.path="/path/to/gossamer/crates/gossamer-binding"'`
to Cargo. gui-gos's `tools/check-rust.sh` takes the SDK `gos env` reports as
`binding_sdk`, or `GOSSAMER_BINDING_PATH` when set.

## License

Apache 2.0. The bundled fonts in `rust/gui-engine/assets/fonts` keep their
own licenses: DejaVu (Bitstream Vera) and IBM Plex Mono (SIL Open Font License
1.1). `SHA256SUMS` there records the source files.
