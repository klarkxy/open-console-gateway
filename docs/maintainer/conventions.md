[简体中文](conventions.zh-CN.md)

# Conventions

- [Architecture](../architecture.md) defines target responsibilities; source and [CLI](../user/cli.md) describe current behavior. A target guarantee is not implemented evidence.
- Preserve supported data, security, and compatibility through [migration](cpa-migration.md). CPA is the sole target execution/retry owner; product services are not another scheduler.
- Configuration, authoritative evidence, execution state, and observations have explicit owners and identity/version checks. Unknown evidence does not become success, zero balance, or a routing ban.
- Rust tests live in sibling tests.rs modules and exercise behavior. Verify the affected boundary.
- Keep paired English/Chinese headings, links, and facts aligned. One architecture document is accompanied by migration and evidence.
- Root README is an entry; capability tables belong in docs/user/, procedures in docs/maintainer/. Retained older guides identify their scope.
- Use portable paths and synthetic examples. Private accounts, host paths, personal model/agent routing, and local tools do not belong in project design.
- Documentation changes require content/link checks; runtime checks accompany implementation. Experiment scope and omissions are recorded, not permanent release exemptions.

[Maintainer guide](../MAINTAINER.md) · [Docs index](../README.md)
