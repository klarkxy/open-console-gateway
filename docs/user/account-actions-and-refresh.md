[简体中文](account-actions-and-refresh.zh-CN.md)

# Account actions and refresh

> Scope: retained current-implementation or published OCG2 operation reference, not the OCG3 design. See [architecture](../architecture.md) for the CPA migration target; UI steps do not apply to this CLI phase.

## Saving and continuing work

A confirmed account or Key save closes its editor without waiting for account lists, model catalogs, or usage to reload. These follow-up reads run in the background. If a read fails, the saved change remains saved; use the affected section's retry action instead of submitting the change again. When local credit setup is part of creating an account, a failed credit setup remains attached to that same account so retrying does not create another.

Background platform observation refreshes do not invalidate an unrelated open configuration editor. A real configuration conflict still keeps the draft for review and never silently retries the write.

## Removing one local Key

Open a Key row's menu on **Accounts** and choose **Delete account**. This action is available for ordinary accounts and for Keys linked to New API or Sub2API. Confirmation removes the selected local account using the existing account deletion endpoint. It does not revoke an upstream Key, close an upstream account, remove sibling Keys, or delete the Provider definition.

**Unlink** is a different operation. It retains the Key and the saved inference endpoint, but removes its platform association. The platform-account deletion guard remains: delete or unlink the associated Keys before removing their parent. There is no implicit cascade deletion.

Once deletion is confirmed by the service, the local account and link are removed immediately. Projection reloads are read-only. A reload failure is reported separately and must not be treated as a reason to submit the deletion again. Earlier in-flight observations cannot restore a deleted row. An authoritative later account-list read can confirm an intentional restoration, such as an import.

The confirmation shows progress while the DELETE is pending and closes as soon as the service confirms deletion. Related lists reload in the background; their latency does not hold the dialog open. A later reload failure reports that deletion already succeeded.

After removing every Key from a configurable HTTP account group, open the group menu and choose **Delete account group**. This explicitly removes its connection configuration, model mappings, and all remaining cards, including the last empty card. The confirmation names the group. A Key on another card of the same group still blocks deletion. System-managed destinations retain their existing restrictions, and New API / Sub2API parent groups keep their existing deletion action. Deleting an individual account does not automatically delete its group.

## Refresh scopes

Use the refresh button beside a Key's enable switch; it is directly accessible without opening the menu. Manual account and platform refreshes share a serial background queue with automatic account refreshes. Waiting buttons show a clock (Queued); the active button shows a spinner (Refreshing). You can queue other accounts and keep using the page. Repeated clicks on the same pending account do not add another request. An error does not stop the next queued account. The queue belongs to the current dashboard session: logout or closing the page discards waiting work, and accounts removed or changed before their turn are skipped. It is not a durable server queue.

For a New API or Sub2API platform, **Refresh** updates the platform observation. On a linked Key, it updates that Key's observation and reports that Key's errors, not the parent's errors. These refresh actions no longer automatically import models for every linked Key. Use the existing **Fetch models** or card-wide model-fetch action to explicitly update model capabilities.

Explicit platform model discovery adds discovered model IDs while preserving existing mappings. A truncated response leaves the saved configuration unchanged and reports the truncation.

For other accounts, manual refresh retains the existing companion model-discovery behavior. A model-only account can run that discovery even when it has no official quota endpoint, without sending an unsupported quota request. The busy indicator covers both the quota request and companion work. A quota failure or rate-limit response does not start further model writes. Automatic quota refresh remains silent, observes its next-eligible time, and does not run companion discovery.

Repeated identical platform refreshes share the pending request. Platform parent and child refreshes do not overlap within that platform. Logout and confirmed removal invalidate pending observations; a late completion cannot populate the new session or release a newer operation's busy flag.

These changes retain the V4 API, its CAS checks, and the existing persisted data format. No migration or upstream account mutation is required.
