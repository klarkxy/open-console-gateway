[简体中文](account-actions-and-refresh.zh-CN.md)

# Account removal and refresh

## Removing one local Key

Open a Key row's menu on **Accounts** and choose **Delete account**. This action is available for ordinary accounts and for Keys linked to New API or Sub2API. Confirmation removes the selected local account using the existing account deletion endpoint. It does not revoke an upstream Key, close an upstream account, remove sibling Keys, or delete the Provider definition.

**Unlink** is a different operation. It retains the Key and the saved inference endpoint, but removes its platform association. The platform-account deletion guard remains: delete or unlink the associated Keys before removing their parent. There is no implicit cascade deletion.

Once deletion is confirmed by the service, the local account and link are removed immediately. Projection reloads are read-only. A reload failure is reported separately and must not be treated as a reason to submit the deletion again. Earlier in-flight observations cannot restore a deleted row. An authoritative later account-list read can confirm an intentional restoration, such as an import.

## Refresh scopes

For a New API or Sub2API platform, **Refresh** updates the platform observation. On a linked Key, it updates that Key's observation and reports that Key's errors, not the parent's errors. These refresh actions no longer automatically import models for every linked Key. Use the existing **Fetch models** or card-wide model-fetch action to explicitly update model capabilities.

Explicit platform model discovery adds discovered model IDs while preserving existing mappings. A partial or truncated response must not erase previously saved models.

For other accounts, manual refresh retains the existing companion model-discovery behavior. A model-only account can run that discovery even when it has no official quota endpoint, without sending an unsupported quota request. The busy indicator covers both the quota request and companion work. A quota failure or rate-limit response does not start further model writes. Automatic quota refresh remains silent, observes its next-eligible time, and does not run companion discovery.

Repeated identical platform refreshes share the pending request. Platform parent and child refreshes do not overlap within that platform. Logout and confirmed removal invalidate pending observations; a late completion cannot populate the new session or release a newer operation's busy flag.

These changes retain the V4 API, its CAS checks, and the existing persisted data format. No migration or upstream account mutation is required.
