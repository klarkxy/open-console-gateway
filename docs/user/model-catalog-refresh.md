[简体中文](model-catalog-refresh.zh-CN.md)

# Model catalog refresh

**Refresh model catalog** on **Providers → Models** updates the saved local directory. It is available for each refreshable sealed Provider and every saved configurable HTTP connection. Refresh is a directory operation, not an inference test: it keeps routing state and quota/cooldown state unchanged.

Newly discovered models are saved enabled with the protocols supported by official documentation or the connection's configured routes. On GOAT's first successful refresh, only plan-included models start enabled; other models in that first snapshot stay off until enabled manually. Models first discovered by later GOAT refreshes use the normal enabled default. Existing saved switches stay in effect. A model with no protocol evidence waits for official documentation; when that evidence later arrives, refresh may add and enable the protocol. Existing mappings, preferred protocol, route overrides, Key grants, and probe observations remain unchanged; refresh may add newly documented protocol declarations. Failed, empty, or stale refreshes preserve the previous directory; a partial response is reported as partial.

For configurable HTTP connections, the configured one to three routes define the available protocols. Refresh uses the saved directory route and, where needed, a ready Key already authorized for that route. It never grants a Key, expands a Key's scope, or proves that a model accepts every configured protocol. Manual mappings remain editable when a service has no compatible model-list endpoint.

## Protocols, tests, and access

The model matrix provides search, an enabled filter, batch enable/disable/delete, protocol preference, per-model test, and refresh. It also keeps the mapping editor for public-name to upstream-ID mappings. Enabling a model enables its declared available protocols; turning it off removes it from routing and `GET /v1/models`.

**Test model** sends one bounded request through the exact saved route and an already authorized ready Key. It does not infer another endpoint, add a grant, change enablement, or change the preferred protocol. Its receipt is tied to the tested scope, Key, and configuration; replacing any of them makes the old observation inapplicable.

An existing single-route connection continues to use its legacy address and authentication. A connection with explicit routes must be edited from **Providers** so the full route set is saved together; Accounts deliberately does not edit its transport.

## Official directories

OpenCode Go reads the public, keyless [`GET /zen/go/v1/models`](https://opencode.ai/zen/go/v1/models) directory and uses the per-model endpoint table in the [Go documentation](https://opencode.ai/docs/go/). For example, `mimo-v2.6-flash` is Chat Completions only. GOAT reads its public [`GET /provider/v1/models`](https://api.commandcode.ai/provider/v1/models) directory and uses each model's documented `supported_endpoints`; documentation takes precedence. The current `xiaomi/mimo-v2.6-flash` entry has Chat Completions and Responses evidence, and does not imply Messages.

MiMo Token Plan can refresh its documented `/models` directory. Its `mimo-v2.6-flash` seed uses Chat Completions, Responses, and Messages. Kimi remains a Chat Completions and Messages Provider. MiniMax CN/API and Global presets expose Chat Completions, Responses, and Messages; the exact saved regional routes and authentication determine a request. Do not infer an undocumented capability merely because another model or Provider supports it.

Price coverage does not control model discovery or selection. Missing prices remain unknown/unpriced, never zero. Refresh pricing separately when needed.
