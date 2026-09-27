/** Remove only confirmed local deletions, never merely missing account overlays. */
export function hideRemovedAccountCredentials<
  C extends { id: string; legacy_account_id: string },
  R extends { credential_ids: string[] },
>(credentials: C[], cards: R[], removedAccountIds: ReadonlySet<string>): { credentials: C[]; cards: R[] } {
  if (removedAccountIds.size === 0) return { credentials, cards };
  const removedCredentials = new Set(credentials
    .filter(row => removedAccountIds.has(row.legacy_account_id)).map(row => row.id));
  if (removedCredentials.size === 0) return { credentials, cards };
  return {
    credentials: credentials.filter(row => !removedCredentials.has(row.id)),
    cards: cards.map(card => {
      const ids = card.credential_ids.filter(id => !removedCredentials.has(id));
      return ids.length === card.credential_ids.length ? card : { ...card, credential_ids: ids };
    }),
  };
}
