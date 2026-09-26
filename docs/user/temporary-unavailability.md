[简体中文](temporary-unavailability.zh-CN.md)

# Temporary Unavailability

On **Settings**, the temporary-unavailability section skips a Key or model locally after a matching upstream error, then retries on real traffic that already uses that route. There is no new navigation item.

Choose **Global** or one connection. A connection rule with the same id replaces the entire global rule; fields are not merged. A disabled local row hides the inherited global rule. Delete the local row to restore inheritance.

## Built-in GOAT credits rejection

The built-in GOAT credits-rejection matcher is sealed. You can disable it for Global or for one connection, or restore inheritance. You cannot turn it into a script, a regular expression, or a raw-body scan.

A classified GOAT insufficient-credits response already failovers this request to the next eligible Key. The Key that reported insufficient credits, plus the actual upstream model, then waits locally. The same Key is not retried on this request.

Other and unknown 400s (context, model, reasoning validation, and similar) stay request-local: this page does not add a first fallback for them. A custom rule can still install a wait that later requests skip; the current unknown-400 request fails as it did before.

GOAT error text still does not create persistent quota or a reset clock. Ordinary `429` handling, Zen Free, and OpenRouter Free stay on their existing paths. See [Routing](routing.md).

## Custom rules

Each custom rule has a scope:

- **Credential** — the wait covers that Key for every model.
- **Credential and model** — the wait binds the actual upstream model sent on that route, not the client alias.

Match fields are optional; any field you fill must be nonempty:

- HTTP status codes `400`–`599`
- exact `error.code` values
- exact `error.type` values
- literal substrings of `error.message`

Filled fields combine with AND. Values inside one field combine with OR. Matching reads only those top-level structured error fields from a bounded JSON error body. It does not walk request echoes, choices, messages, or successful bodies, and it does not scan the raw body or run regular expressions. A status-only rule can match without a parsed body. A body that cannot be read does not become synthetic error text.

## Local waits

Active restrictions show `waiting`, `ready`, or `probing`:

- **waiting** — skip this target until the local backoff is due.
- **ready** — this local rule allows a re-probe of that source. The send still has to pass other rules, a valid `Retry-After`, and official limits. Local backoff expiring, including the 30-second default, does not mean the request will go out.
- **probing** — that request is in flight.

The list is each source's local wait, not overall eligibility.

A complete, protocol-valid JSON or SSE success clears only the restriction that holds this request's probe lease and was not updated concurrently. Success on another model does not clear it. There is no background probe and no periodic poller. Only traffic that actually matches the waiting route is sent again.

**Clear local wait** drops that process-local wait. It does not send a request, change enablement, auth, quota recovery, or an upstream `Retry-After`, and it does not mean the upstream is healthy. An empty restriction list also does not mean the upstream is healthy. Reading the list does not probe.

If every compatible Key is waiting only on local policy, the gateway returns `503`. All-waiting `429` / `Retry-After` behavior is unchanged.

Backoff defaults are 30 and 300 seconds. Each value is an integer from 1 through 86400, and the maximum must be at least the initial.

## Persistence and copies

Saved rules survive restart. Process-local waiting, ready, and probing do not; after a restart the next matching request can send again. Existing official quota recovery is a separate mechanism and is not created from these error bodies.

A full data-directory backup keeps the saved rules. Portable account export does not carry them, and import does not clear unrelated local rules on the target. After you delete a connection, its rules are removed or made inactive in that same change; they are not rebound by display name.

## Limits

- 128 configured rules in total
- at most 32 enabled rules effective for any one connection
- at most 32 values per match field
- at most 256 UTF-8 bytes per match string

---

[User guide index](../USER.md) · [简体中文](temporary-unavailability.zh-CN.md) · [Docs index](../README.md)
