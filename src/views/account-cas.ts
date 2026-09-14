/**
 * Reconcile the account open in the edit modal against a reloaded list after
 * a control-plane conflict: returns the fresh copy when it still exists, or
 * null when it was deleted elsewhere. The caller must close the edit modal on
 * null — a null editing account would otherwise flip the modal into create
 * mode, since the form derives edit-vs-create from account presence.
 */
export function reconcileEditingAccount<T extends { id: string }>(
  accounts: readonly T[],
  editingId: string | null | undefined,
): T | null {
  if (!editingId) return null;
  return accounts.find(({ id }) => id === editingId) ?? null;
}
