[简体中文](data-security.zh-CN.md)

# Data And Security

Open Console Gateway stores your keys, passwords, and browser sessions on the local disk.
Protect the data directory: there is no remote recovery if it is lost.

- **GUI data location.** Windows: `%USERPROFILE%\.ocg-mgr`. macOS / Linux:
  `~/.ocg-mgr`. CLI data defaults to `~/.ocg-mgr-cli` on every platform and
  can be overridden with `--data-dir <path>`.
- **Credential storage.** Account keys and saved login passwords are stored
  with AES-256-GCM (`v2:` ciphertext) derived from the Host cipher seed.
  Older XOR-obfuscated rows still decrypt so a directory backup remains
  restorable; a successful open rewrites them to v2. This is still a
  local-disk bound, not a remote KMS: anyone with the data directory and its
  `.encryption-key`, or able to run the Windows GUI in the original Windows
  user/machine context, can recover account keys and saved login passwords.
  Dashboard Access Keys live in `access_keys` (schema v27). The macOS /
  Linux GUI and the CLI also place a `.encryption-key` file inside the data
  directory; **back it up with the database** because losing it makes stored
  credentials unreadable. The dashboard SPA never writes Key plaintext to
  `localStorage`; Connection Center secrets stay in memory until logout or
  401. Probe and repair errors do not print plaintext Keys.
- **Browser profiles.** `browser-profiles/`, or Docker's
  `ocg-browser-profiles`, contains long-lived cookies and official-site login
  state and is not encrypted by Open Console Gateway at all. Protect, transfer, and
  destroy it with the same care as the database and account keys.
- **Portable node backup.** Each node manages its own accounts through its own
  dashboard. Move portable node state with a password-encrypted `.ocgbackup`
  file from the loopback dashboard; no separate administrator step-up is
  required. Account and Access Keys are encrypted with Argon2id plus
  AES-256-GCM. The migration password is not stored and cannot be recovered.
  Treat the file and password as separate secrets. Browser profiles, login
  passwords, logs, usage, source cooldown state, and machine-local host
  settings are not included.
- **Plain HTTP warning.** A non-loopback `http://` root URL exposes the Key
  and request contents to the network. Use HTTPS or a trusted LAN only.
- **Administrator password.** The single administrator password is stored as
  an Argon2 hash in SQLite. There is no self-service password recovery —
  protect the data directory.
- **Custom API destinations.** Complete Custom inference Endpoints are
  administrator-trusted. Public, LAN, and loopback HTTP or HTTPS destinations
  are allowed. Metadata, link-local, and opaque IPv4-trick hosts are rejected.
  URL-embedded credentials are rejected; query strings and fragments are
  rejected; secret-bearing requests never follow redirects; dashboard and
  client credentials are never forwarded. A model override to another Origin
  does not inherit the stored Key. Choose destinations you intend to reach
  from this node.

---

[User guide index](../USER.md) · [简体中文](data-security.zh-CN.md) · [Docs index](../README.md)
