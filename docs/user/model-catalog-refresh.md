[简体中文](model-catalog-refresh.zh-CN.md)

# Go model catalog refresh

Refresh the model catalog on Providers to fetch the public OpenCode Go directory. This request sends no account Key and can run without a configured Go account. It does not test credentials, make an inference request, or change account quota/cooldown state.

New entries appear in the saved directory and start disabled. Select and enable an upstream protocol before using them. Known offline defaults and recognized official documentation provide protocol hints. If a document cannot be read or omits a model, existing evidence and saved preferences survive; an entirely unknown model is not assumed to support Chat Completions. A model's mandatory preferred-protocol field alone is not proof of support; inspect its available/enabled protocols.

After enablement, new exact Go IDs can appear in the gateway model list, application picker, and Aliases page without a release. They remain provider-pinned names, not automatically shared aliases. Conflicting raw names are withheld from the client list and rejected as ambiguous. Aliases publication switches only hide names; they do not disable routing. Gateway and inference authentication are unchanged.

Price coverage does not control model discovery or selection. Missing prices remain unknown/unpriced, never zero. Refresh pricing separately when needed.
