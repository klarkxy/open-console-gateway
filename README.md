[简体中文](README.zh-CN.md)

# Open Console Gateway — OCG3

`ocg3` and `open-console-gateway` are two generations of this same project, in the way Python 3 and Python 2 are two generations of Python. This tree is the OCG3 generation. The product name remains Open Console Gateway. The CLI command is `ocg`. The Rust package is `ocg-cli`.

OCG3 is a local multi-Plan gateway. Its execution target is one local CPA runtime owned by this process, not a connection to a separate CPA product. That CLI is still incomplete, so the native desktop GUI stays postponed. No WebUI or WebView-based main interface is planned.

The default data directory for `ocg` is `~/.ocg3`. The command does not open, move, or delete a previous generation's `~/.ocg-mgr-cli`. Pass `--data-dir` to choose a directory.

- [Architecture](docs/architecture.md) — Responsibilities, request flow, quotas, retries, complete CLI acceptance, and native GUI design.
- [Migration boundaries](docs/maintainer/cpa-migration.md) — Retained capabilities, CPA responsibilities, compatibility, and rollback.
- [CPA findings](docs/maintainer/cpa-validation.md) — Completed experiments and unimplemented guarantees.
- [Current CLI guide](docs/user/cli.md) — Build, configure, and operate this checkout.
- [Documentation](docs/README.md) · [Maintainer guide](docs/MAINTAINER.md).

Published OCG2 installers and desktop guides describe earlier releases. They are not evidence that CPA-based OCG3 is available.

[Contributors](docs/CONTRIBUTORS.md) · [License](LICENSE)
