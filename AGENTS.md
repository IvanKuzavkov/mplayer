# AGENTS.md file

## Dev environment tips

- use rust 2024 edition
- Use `cargo fmt` and `cargo clippy` to stay in right formating and check for rust guidlines.
- Do not bringh new dependencies without approval as well as change dependency version and feature set. To manage dependencies use `cargo add`.
- Always as possible write a tests to unittest logic. Use `cargo test` and `[dev-dependencies]` to work with test dependencies.
- Use minimal possible code and use ideomatic Rust construction and patterns.
- Do not touch files and move it out of the original location without a permission.
