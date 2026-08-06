# Security policy

## Supported versions

Security fixes are applied to the latest release on `main`.

## Reporting a vulnerability

Please use GitHub's **Security → Report a vulnerability** flow rather than a public issue. Include the affected version, impact, reproduction steps, and any suggested mitigation. You can expect an acknowledgement within seven days.

Do not include Slack signing secrets, bot tokens, GitHub tokens, or private repository URLs in a report.

## Operational checklist

- Use a fine-grained GitHub token scoped to the smallest possible repository set.
- Give the token an expiration date and rotate it after suspected exposure.
- Keep `ALLOW_ALL_USERS=false` and allowlist Slack member IDs explicitly.
- Keep `ALLOW_CHANNEL_MESSAGES=false` unless public-channel operation is required.
- Start with `DRY_RUN=true` while configuring Slack.
- Terminate TLS before the service; it intentionally exposes plain HTTP inside the trusted runtime boundary.
- Treat logs as sensitive metadata even though credentials are never logged.
- Run a single replica, or add a shared replay/deduplication layer in front of multiple replicas.

## Known boundaries

- Authorization proves that Slack delivered the event for an allowed workspace user. It does not assess whether the PR is safe or correct.
- The in-memory replay cache resets when the process restarts.
- GitHub remains the ultimate policy enforcement point for branch protection, required checks, review dismissal, and merge authorization.
- Optional comments require Issues write permission because GitHub models PR conversation comments as issue comments.
