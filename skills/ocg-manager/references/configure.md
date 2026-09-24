# Configure the local Gateway

Read [secret handling](secrets.md) before a Key, password, backup passphrase, cookie, or authenticated response can enter a tool. Let the user enter secrets directly in their local dashboard or destination client's secret field. The agent can prepare all nonsecret choices, navigate to the right form, and resume after the user confirms completion.

## Actual control surfaces

Run the installed `ocg-manager-cli --help` and the relevant subcommand's `--help` for current syntax and account scope. In particular, `key add` and `key ping` are OpenCode Go-only, while ID-based `key remove/enable/disable` may affect other Providers. Confirm account identity in the dashboard before an ID mutation. `key ping` makes a real upstream request and may return an upstream body excerpt. For other Providers, Gateway Access Keys, Custom endpoints, models, proxy, and routing, use the dashboard.

Do not run CLI commands that carry or reveal secrets through agent tools; see [secret handling](secrets.md). The CLI has no separate Admin API or direct dashboard Settings command.

## First useful route

1. Open `/dashboard/` on the node. For a native loopback listener, the local panel normally skips admin login. A Docker listener is non-loopback **inside the container**, so create or log in as the administrator even when the host port is bound to `127.0.0.1`.
2. In **Accounts**, select the actual Provider or Plan, inspect its endpoint and model contract, and let the user type any upstream Key. A ready account is enabled on creation; confirm its displayed state. Zen Free uses its own account switch and saved Free model catalog.
3. In **Providers**, refresh the relevant catalog or set models/protocols only if this route requires it. A Custom API or user-defined HTTP Provider needs a saved endpoint, a supported protocol, public-to-upstream model mapping, and the correct per-Key destination/Origin grant. Do not treat a connection test as protocol or billing proof.
4. Set account card order, routing strategy, and conversation affinity in **Accounts** as requested. Set outbound proxy, port, client root, and timeouts in **Settings**. Do not write an environment override back into saved Settings; its effect may be read-only.
5. In the **Access Center**, the user copies an enabled Gateway Key directly into the target client. Supply the displayed full API Base URL, normally `http://127.0.0.1:9042/v1`, and the client's supported OpenAI, Anthropic, or Gemini format. The client Key is distinct from each upstream account Key.
6. Have the user send one small authenticated client request using an actually published model. Inspect the resulting dashboard request log for route, status, and error without exposing request headers, body, or secret fields. A live upstream request may consume quota or money.

Native desktop or CLI on the same OS user can install the OCG DSH plugin from **Applications > DSH** when that integration is requested; the Host passes the selected Access Key through a local handoff rather than a command argument. Docker cannot install a plugin into the browser's or host's DSH. For other clients, guide manual configuration without asking for their stored secret.
