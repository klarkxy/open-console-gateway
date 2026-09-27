import type { AccountMenuOption } from "./account-display.ts";

/** Complete lifecycle actions at the credential row, including platform Keys. */
export function accountRowActions(
  options: readonly AccountMenuOption[],
  target: { id: string; name: string } | null,
  capabilities: { platformLinked: boolean; refreshSupported: boolean; deleting: boolean; refreshBusy?: boolean },
): AccountMenuOption[] {
  const result = options.filter(option => option.key !== "refresh-usage" || capabilities.refreshSupported);
  if (target && capabilities.platformLinked && !result.some(option => option.key === "delete")) {
    result.push({
      key: "delete",
      accountId: target.id,
      accountName: target.name,
      disabled: result.find(option => option.key === "unlink")?.disabled ?? false,
    });
  }
  return result.map(option => ({
    ...option,
    disabled: Boolean(option.disabled || capabilities.deleting
      || (option.key === "refresh-usage" && capabilities.refreshBusy)),
  }));
}
