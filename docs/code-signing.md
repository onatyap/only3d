# Code signing policy

Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by
[SignPath Foundation](https://signpath.org).

**Status:** only3d has applied to SignPath Foundation. Until the application is accepted, the
Windows downloads are unsigned (the release notes say how to open them). macOS builds are signed
ad hoc, not with an Apple Developer ID.

## What is signed

The Windows downloads of each release: `only3d.exe`, `only3d-app.exe` and the installer
(`only3d-<version>-windows-x64-setup.exe` and `-arm64-setup.exe`). They are built from this
repository's source by [the release workflow](../.github/workflows/release.yml) on
GitHub-hosted runners, and signed only after a team member approves each signing request.

## Team

only3d is maintained by one person, who holds every role:

| Role | Who |
|---|---|
| Committers and reviewers | [onatyap](https://github.com/onatyap) |
| Approvers | [onatyap](https://github.com/onatyap) |

Changes from anyone else come as pull requests and are reviewed before they're merged
([CONTRIBUTING.md](../CONTRIBUTING.md)). Everyone with these roles uses multi-factor
authentication on GitHub and SignPath.

## Privacy policy

only3d has no telemetry. What it sends over the network, and how to turn each part off, is in
[docs/privacy.md](privacy.md).
