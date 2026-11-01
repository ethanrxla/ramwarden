## What changes for the user

## Platforms
- [ ] macOS
- [ ] Windows
- [ ] Linux
<!-- Any platform left unticked needs a linked issue. -->

## Rubric (docs/RUBRIC.md)
- [ ] Platform code is behind the platform trait and `cfg(target_os)`
- [ ] Nothing protected can be closed; the trim → suspend → quit → kill ladder holds
- [ ] `cargo fmt`, `cargo clippy -D warnings` and `cargo test` pass
- [ ] New decision logic and bug fixes have tests
- [ ] No secrets, no blocking calls on the monitor loop, every external call has a timeout
- [ ] README / `ramwarden.toml` updated if behaviour changed

## How it was tested
