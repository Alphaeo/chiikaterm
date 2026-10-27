# Contributing to chiika

Thanks for your interest! Bug reports, ideas and pull requests are welcome.

## Before you start

- **A bug**: open an issue with the *Bug report* form; `chiika --version`, your OS and terminal
  and the steps to reproduce make it possible to fix.
- **A feature**: open an issue first to talk about it, so that your work fits where chiika goes.
- **A security issue**: do not open a public issue, see [SECURITY.md](SECURITY.md).

## Build and test

You need Rust (see [the README](README.md#1-prerequisites)). Then:

```sh
cargo run                                   # chiika, from this folder
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo deny check                            # licenses and known vulnerabilities of dependencies
```

CI runs the first four on Windows, Linux and macOS for every push and pull request, and
`cargo deny` once; a pull request is merged when they pass.

chiika is developed on Windows with Windows Terminal: if you change something on another OS or
terminal, say in the pull request where you tried it.

## Pull requests

- One change per pull request, as small as it can be; no unrelated reformatting.
- Follow the code around you: naming, structure, how much it comments.
- Add a test for a fix or a new behaviour when it can be tested.
- Add a line under `## [Unreleased]` in [CHANGELOG.md](CHANGELOG.md) for anything users notice.
- Short, descriptive commit messages.
- A new dependency needs a reason in the pull request: chiika keeps few of them.

## License

By contributing, you agree that your contributions are licensed under the project's licenses,
[MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at the user's option.
