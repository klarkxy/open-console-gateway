[简体中文](platform-accounts.zh-CN.md)

# New API and Sub2API accounts

Each site offers **Add Key** and **Link existing Key**. Add Key opens a Custom API form with the site's inference address; the selected protocol determines its path. Saving creates the Key and then associates that exact account with the site. If association fails after creation, the form shows that the Key already exists and retries only association. Reloading keeps the standalone Key available under Link existing Key. An uncertain creation result must be reconciled against the account list before creating again.

**New API** and **Sub2API** are platform types, not singleton suppliers. Add a separate named instance for each site or user account: multiple New API instances and multiple Sub2API instances can coexist. Each instance has its own identity, site URL, credentials, linked Keys and observations. Refreshing, editing or deleting one instance does not act on every instance of that type.

On **Accounts**, open **Add account**, choose **New API** or **Sub2API** under **Platform accounts**, then enter the site URL and a name. The platform accounts section appears after an account is created. A parent groups your existing Custom API Keys; each Key keeps its model mappings, protocol, enablement, cooldown, and position in the global route order. Grouping does not create a new Provider or change fallback priority.

Use the site's root URL, including any installation path. The platform and URL are fixed after creation. Link an existing Custom API Key explicitly, or add a Custom API account with the Key you paste and then link it. OCG does not fetch remote Key secrets. A manual association is not proof of remote ownership or model permission. A linked Key is recorded as a declared (not verified) relation to the platform account.

An optional ordinary user credential enables user-scoped observations. It is stored separately from inference Keys and is never returned by the dashboard or included in a transfer bundle. Omitting it keeps Key-scoped reads available. Expired user credentials do not disable inference. Clearing the credential removes cached observations that depended on it.

## Refresh and read the results

Refresh is manual. Parent observations show the account wallet and subscriptions; Key observations show that Key's limits and consumption. Values carry their source, unit, period, and observation time. Wallet, subscription, Key, daily, weekly, and monthly limits are separate scopes: do not add them together. A missing field stays unknown. Wallet or subscription data returned through a Sub2API Key stays labeled as observed through that Key; a manual parent association does not establish shared ownership.

New API supports wallet and subscription billing, fixed and automatic groups, and billing preferences. Sub2API's billing endpoint describes Key multipliers; it is not a balance endpoint. Model-plaza prices are display data and do not establish Key access. Fixed model permissions or Key-authenticated model discovery supply candidates; select and confirm models before importing them. Refreshing prices never changes routeable models automatically.

Price estimates use fixed text-token rates only when the model, group, unit, and multipliers are known. Automatic groups, expressions, tiered prices, per-request/image rates, and unresolved peak rules remain unavailable for request estimates. Prices expire within 24 hours. Enabled peak/time-varying rates are display-only in this version. Anonymous New API rates cannot resolve user-specific multipliers and do not produce request estimates. A failed refresh marks prior observations stale and prevents their use for new estimates.

Each upstream attempt freezes its own price evidence. A fallback Key uses its own rate, and later refreshes do not rewrite history. Logs retain the estimated amount in its original currency and price provenance. An estimate does not establish actual wallet debit, subscription deduction, or whether a failed request was charged.

## Change or transfer an account

A linked Key's endpoint belongs to its parent. Unlinking keeps the Key, its materialized endpoint, and model configuration. Delete or unlink all children before deleting a parent; parent deletion never deletes Keys.

New node exports use payload V5. They include parent definitions and associations alongside existing portable inference configuration, but exclude management credentials and observation snapshots. V4 bundles remain importable. Imported groups are unverified until fresh evidence is obtained. Import rejects a matching parent ID with a different platform or site URL as one atomic failure.

Back up the complete data directory before upgrading. Rollback restores that full backup; do not open a migrated database with an older binary. See [storage and migrations](../maintainer/storage-migration.md).

The reader baseline is New API `71c1fd7caad738db4d13aabbf28eeadb293d0cfe` and Sub2API `772a0382f079676983c06f24b0d41e09139a8462`. Older releases and forks may omit or change these interfaces; an unavailable observation is not an inference failure.
