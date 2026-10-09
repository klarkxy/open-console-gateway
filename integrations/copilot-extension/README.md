# Open Console Gateway for VS Code
Install the OCG-owned VSIX from Applications → VS Code Copilot, then open/reload the selected VS Code Profile. Models appear under Open Console Gateway in Chat. This is a local UI extension, including for remote workspaces; it does not install into a remote extension host.

The extension fetches authenticated `/v1/models` on discovery and before each request. Context/output limits and capabilities come from OCG. Models with missing token metadata are listed in the OCG application status and **OCG: Refresh Models**; complete their model capabilities in OCG Aliases once. No global context budget is imposed.

Model names in the picker are OCG public aliases, also used in requests. Different aliases remain separate even when they share an upstream display name.

For a manual install, run **OCG: Connect** and enter the OCG `/v1` URL and enabled Key. **OCG: Disconnect** clears this extension's encrypted connection. OCG-managed uninstall first requests a disconnect and waits for acknowledgment; open the selected Profile and retry if it is pending. OCG Keys themselves remain in OCG.

Chat Completions, Responses, and Messages use the published preferred protocol. Text, tool calls/results, and image inputs follow the declared capabilities. Token counting uses o200k text encoding with message/tool overhead and conservative image estimates; non-OpenAI tokenization may differ. These estimates do not change OCG's declared model limits. Private thinking signatures are kept in a bounded in-memory replay cache for matching assistant turns and are never rendered as assistant text. Disconnect, reload, or Key replacement clears this cache. The provider supports Chat/agents, not inline completions or Next Edit Suggestions.

`pnpm install --frozen-lockfile && pnpm run build:copilot && pnpm run test:copilot` rebuilds the embedded runtime. `pnpm run check:copilot` detects a stale bundle. Dependencies are scoped to this extension and pinned by the workspace lockfile; the native host builds a VSIX from the checked-in runtime without Node at install time.

**OCG: Set Reasoning Effort** selects a model and an effort declared by its current OCG metadata. The preference is per model and Profile; Messages never receives a guessed thinking budget.
