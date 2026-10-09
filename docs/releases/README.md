[简体中文](README.zh-CN.md)

# Release Notes

Per-version upgrade, change, and verification notes. Each entry states whether the release needs a database migration, what changed, and what was actually verified. Upgrading, backing up, and rolling back are covered once in the [upgrade and backup guide](../user/upgrade-backup.md) rather than repeated per version.

The current version is **2.11.0**. Installers and images are published on the [GitHub Releases page](https://github.com/klarkxy/open-console-gateway/releases/latest).

| Version | Notes | Schema |
| --- | --- | --- |
| 2.11.0 | [English](v2.11.0.md) · [简体中文](v2.11.0.zh-CN.md) | Migrates v66 → v67 |
| 2.10.0 | [English](v2.10.0.md) · [简体中文](v2.10.0.zh-CN.md) | No migration (stays v66) |
| 2.9.0 | [English](v2.9.0.md) · [简体中文](v2.9.0.zh-CN.md) | No migration (stays v66) |
| 2.8.0 | [English](v2.8.0.md) · [简体中文](v2.8.0.zh-CN.md) | Migrates v64 → v66 |
| 2.7.0 | [English](v2.7.0.md) · [简体中文](v2.7.0.zh-CN.md) | No migration (stays v64) |
| 2.6.4 | [English](v2.6.4.md) · [简体中文](v2.6.4.zh-CN.md) | Migrates to v64 |
| 2.6.3 | [English](v2.6.3.md) · [简体中文](v2.6.3.zh-CN.md) | No migration (stays v63) |
| 2.6.2 | [English](v2.6.2.md) · [简体中文](v2.6.2.zh-CN.md) | No migration (stays v63) |
| 2.6.1 | [English](v2.6.1.md) · [简体中文](v2.6.1.zh-CN.md) | No schema change |
| 2.6.0 | [English](v2.6.0.md) · [简体中文](v2.6.0.zh-CN.md) | Migrates v62 → v63 |
| 2.5.0 | [English](v2.5.0.md) · [简体中文](v2.5.0.zh-CN.md) | Migrates to v62 |
| 2.4.2 | [English](v2.4.2.md) · [简体中文](v2.4.2.zh-CN.md) | Not stated in the entry |

Each version has its own entry rather than a combined changelog, so that an upgrade note stays accurate after the code moves on. Older versions before 2.4.2 are covered by the repository's tag history.

---

[Maintainer guide index](../MAINTAINER.md) · [简体中文](README.zh-CN.md) · [Docs index](../README.md)
