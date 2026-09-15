[简体中文](applications.zh-CN.md)

# Applications

**Applications** is the dashboard's downstream-integration page. It currently
hosts the **DSH** tab, which connects DSH itself to the gateway.

## DSH

The **DSH** tab installs the OCG-owned plugin into DSH's `web` profile.

1. Run either the installed Open Console Gateway Desktop app or the native
   `ocg-manager-cli serve` build on the same machine and as the same OS user as
   DSH `0.1.5-rc.1` or `0.1.5-rc.2`.
2. Open **Applications > DSH** and choose an enabled OCG Key.
3. Select **Install**. Review the detected DSH version and the exact local
   targets in the confirmation dialog, then confirm.
4. Start or restart DSH when the page asks you to do so.

The browser sends only the selected Key id to the authenticated Dashboard API.
The Desktop host resolves the value, writes it to a private one-time handoff
file, and invokes DSH's official `plugin --profile web add` flow. When DSH
loads the plugin, the plugin imports that value into DSH's own credential
service and removes the handoff file. The Key is never placed in the generated
package source or command arguments.

The plugin registers one **Open Console Gateway** provider and refreshes the
authenticated `GET /v1/models` list when DSH asks for models or prepares a
call. That is the full set of public names currently offered to clients,
including eligible Custom IDs; it is not the narrower dashboard
`application-models` list. Model visibility changes therefore do not require
reinstalling the plugin.

An **Installed** status proves the package registration and credential handoff
were prepared. It does not prove that DSH has restarted, loaded the plugin, or
completed a real model call. After restart, choose an OCG model in DSH, send a
request, and confirm it in OCG **Logs**. When installation is blocked
(unsupported environment, missing DSH, incompatible version, or a conflict),
the page also shows the host's specific reason next to the status.

Installation spans two local stores, so rollback has one deliberate limit. If
DSH imports the Key and a later install step fails, OCG can restore the plugin
registration and any still-pending handoff, but it cannot prove that DSH's
credential write did not already commit. To retry, stop DSH, reopen the page,
and use **Reinstall** with the intended Key; then start DSH once and confirm a
request in **Logs**. To remove the integration, stop DSH, run
`dsh plugin --profile web remove @open-console-gateway/dsh-plugin`, and remove
only `OCG_GATEWAY_KEY` in DSH's credential settings (or the `refs` entry in
`<DSH_HOME>/.credentials.yaml`). If you are deliberately abandoning a pending
activation, remove the `credential-handoff` and matching `.claimed-*` files
shown by the page only while DSH is stopped. Removing the plugin does not
disable the OCG Key itself; rotate or disable that Key in OCG when required.

The native headless CLI supports this installation against the DSH on its own
host. The official Docker image reports local installation as unsupported: a
container cannot install into the browser user's or Docker host's DSH. It may
still serve DSH through ordinary Gateway configuration.

[User guide index](../USER.md) · [Manual client setup](add-application.md) · [Docs index](../README.md)
