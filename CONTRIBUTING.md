# Contributing

Thanks for improving `approve-bot`.

## Local setup

Install Rust 1.85 or newer, then run:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

No live Slack or GitHub credentials are required. HTTP integrations use local mock servers.

## Pull requests

- Keep the approval policy obvious and auditable.
- Add tests for behavior changes, especially authentication and authorization paths.
- Avoid dependencies that duplicate standard-library functionality.
- Never add real credentials, private repository URLs, or Slack workspace data to fixtures.
- Update `README.md` and `.env.example` when configuration changes.

Please open an issue before a large redesign. Small fixes and focused improvements are welcome directly.

By participating, you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
