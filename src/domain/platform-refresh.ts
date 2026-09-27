/** Resolve only the requested observation scope; a child never borrows parent errors. */
export function platformRefreshErrors(
  parents: readonly { id: string; snapshot: { errors: readonly string[] } | null }[],
  links: readonly { accountId: string; platformAccountId: string; snapshot: { errors: readonly string[] } | null }[],
  parentId: string,
  accountId?: string,
): string[] {
  const snapshot = accountId === undefined
    ? parents.find((parent) => parent.id === parentId)?.snapshot
    : links.find((link) => link.accountId === accountId && link.platformAccountId === parentId)?.snapshot;
  return [...(snapshot?.errors ?? [])];
}
