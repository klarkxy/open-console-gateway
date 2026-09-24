import { computed, ref } from "vue";
import type { Ref } from "vue";
import type { MessageApi } from "naive-ui";
import { DashboardRequestError, dashboardApi } from "../api/dashboard.ts";
import type { Account } from "../api/dashboard.ts";
import { moveItem } from "../domain/account-lifecycle.ts";
import { expandGroupOrder, type DestinationGroup } from "../domain/destination-groups.ts";
import { t } from "../i18n/index.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";

type AccountDragState = {
  accountId: string;
  handle: HTMLElement;
  moved: boolean;
  pointerId: number;
  previous: Account[];
  items: DestinationGroup[];
};

function sameAccountOrder(left: readonly Account[], right: readonly Account[]): boolean {
  return left.length === right.length && left.every((account, index) => account.id === right[index]?.id);
}

function accountsFromGroups(
  groups: readonly DestinationGroup[],
  previous: readonly Account[],
): Account[] {
  const expanded = expandGroupOrder(groups);
  const byId = new Map(previous.map((account) => [account.id, account]));
  const next = expanded
    .map((id) => byId.get(id))
    .filter((account): account is Account => Boolean(account));
  const seen = new Set(next.map((account) => account.id));
  return [...next, ...previous.filter((account) => !seen.has(account.id))];
}

function resolveGroupId(
  targetId: string | undefined,
  groups: readonly DestinationGroup[],
): string | undefined {
  if (!targetId) return undefined;
  if (groups.some((group) => group.id === targetId)) return targetId;
  return groups.find((group) => group.credentials.some((credential) => (
    credential.id === targetId || credential.legacy_account_id === targetId
  )))?.id;
}

/**
 * Pointer-drag and keyboard reordering for the ordered account card list,
 * including persistence, screen-reader announcements, and 409 conflict
 * reconciliation against the server list.
 */
export function useAccountOrder(options: {
  accounts: Ref<Account[]>;
  previewOrder: Ref<string[] | null>;
  message: Pick<MessageApi, "success" | "warning" | "error">;
  refreshAfterDetachedSave: () => Promise<unknown>;
  busy: Ref<boolean>;
  reloadAfterRevisionConflict: () => Promise<void>;
  groups: Ref<DestinationGroup[]>;
  afterSave?: () => Promise<unknown>;
}) {
  const {
    accounts,
    busy,
    reloadAfterRevisionConflict,
    groups,
  } = options;
  const { message, previewOrder } = options;
  const displayedAccounts = computed(() => {
    const rank = new Map((previewOrder.value ?? []).map((id, index) => [id, index]));
    return previewOrder.value
      ? [...accounts.value].sort((left, right) => (rank.get(left.id) ?? Infinity) - (rank.get(right.id) ?? Infinity))
      : accounts.value;
  });
  const preview = (list: readonly Account[]) => { previewOrder.value = list.map((account) => account.id); };

  const orderSaving = ref(false);
  const draggingAccountId = ref<string | null>(null);
  const orderAnnouncement = ref("");
  let accountDrag: AccountDragState | null = null;
  let disposed = false;

  function currentGroups(): DestinationGroup[] {
    return groups.value;
  }

  function applyGroups(nextGroups: readonly DestinationGroup[], previous: readonly Account[]): void {
    preview(accountsFromGroups(nextGroups, previous));
  }

  function sortableLength(): number {
    return currentGroups().filter((group) => group.credentials.length >= 1).length;
  }

  function clearAccountDrag(state: AccountDragState): void {
    window.removeEventListener("pointermove", previewAccountDrag);
    window.removeEventListener("pointerup", finishAccountDrag);
    window.removeEventListener("pointercancel", cancelAccountDrag);
    accountDrag = null;
    draggingAccountId.value = null;
    if (state.handle.hasPointerCapture(state.pointerId)) {
      state.handle.releasePointerCapture(state.pointerId);
    }
  }

  async function persistAccountOrder(previous: Account[], movedItemId: string): Promise<void> {
    if (sameAccountOrder(previous, displayedAccounts.value)) { previewOrder.value = null; return; }
    orderSaving.value = true;
    try {
      const saved = await dashboardApi.reorderAccounts(displayedAccounts.value.map(({ id }) => id));
      if (disposed) {
        await options.refreshAfterDetachedSave();
        return;
      }
      accounts.value = saved;
      previewOrder.value = null;
      // The order is already committed. A failed revalidation must not roll
      // back the successful receipt to the pre-write list.
      try { await options.afterSave?.(); } catch { /* The destination store exposes its load error. */ }
      if (disposed) return;
      const items = currentGroups();
      const moved = items.find((item) => item.id === movedItemId);
      const position = items.findIndex((item) => item.id === movedItemId) + 1;
      const name = moved?.credentials[0]?.name || moved?.destination.name;
      if (name && position > 0) {
        orderAnnouncement.value = t("账号 {name} 已移至第 {position} 位", {
          name,
          position,
        });
      }
      message.success(t("账号顺序已更新"));
    } catch (error) {
      if (disposed) return;
      previewOrder.value = null;
      if (error instanceof DashboardRequestError && error.status === 409) {
        try {
          await reloadAfterRevisionConflict();
        } catch {
          // The optimistic preview was already reverted; the error below asks
          // the user to retry after an explicit refresh if reconciliation fails.
        }
        if (disposed) return;
        const conflict = t("账号设置已被其他操作修改，已重新加载最新状态，请重试");
        orderAnnouncement.value = conflict;
        message.warning(conflict);
        return;
      }
      const failure = t("保存账号顺序失败：{error}", { error: dashboardErrorDetail(error) });
      orderAnnouncement.value = failure;
      message.error(failure);
    } finally {
      orderSaving.value = false;
    }
  }

  function startAccountDrag(event: PointerEvent, accountId: string): void {
    if (
      disposed
      || orderSaving.value
      || busy.value
      || sortableLength() < 2
      || accountDrag !== null
      || !event.isPrimary
      || (event.pointerType === "mouse" && event.button !== 0)
    ) return;
    const handle = event.currentTarget as HTMLElement;
    event.preventDefault();
    handle.setPointerCapture(event.pointerId);
    accountDrag = {
      accountId,
      handle,
      moved: false,
      pointerId: event.pointerId,
      previous: [...displayedAccounts.value],
      items: currentGroups(),
    };
    draggingAccountId.value = accountId;
    window.addEventListener("pointermove", previewAccountDrag, { passive: false });
    window.addEventListener("pointerup", finishAccountDrag);
    window.addEventListener("pointercancel", cancelAccountDrag);
  }

  function previewAccountDrag(event: PointerEvent): void {
    const state = accountDrag;
    if (!state || state.pointerId !== event.pointerId) return;
    event.preventDefault();
    const target = document
      .elementFromPoint(event.clientX, event.clientY)
      ?.closest<HTMLElement>(".account-card[data-account-id]");
    const targetId = resolveGroupId(target?.dataset.accountId, state.items);
    if (!targetId || targetId === state.accountId) return;
    const fromIndex = state.items.findIndex((item) => item.id === state.accountId);
    const toIndex = state.items.findIndex((item) => item.id === targetId);
    if (fromIndex < 0 || toIndex < 0 || fromIndex === toIndex) return;
    const targetItem = state.items[toIndex];
    if ((targetItem?.credentials.length ?? 0) === 0) return;
    const sourceItem = state.items[fromIndex];
    if ((sourceItem?.credentials.length ?? 0) === 0) return;
    state.items = moveItem(state.items, fromIndex, toIndex);
    applyGroups(state.items, state.previous);
    state.moved = true;
  }

  async function finishAccountDrag(event: PointerEvent): Promise<void> {
    const state = accountDrag;
    if (!state || state.pointerId !== event.pointerId) return;
    event.preventDefault();
    clearAccountDrag(state);
    if (!state.moved || sameAccountOrder(state.previous, displayedAccounts.value)) return;
    await persistAccountOrder(state.previous, state.accountId);
  }

  function cancelAccountDrag(event: PointerEvent): void {
    const state = accountDrag;
    if (!state || state.pointerId !== event.pointerId) return;
    event.preventDefault();
    previewOrder.value = null;
    clearAccountDrag(state);
  }

  async function handleOrderKeydown(event: KeyboardEvent, accountId: string): Promise<void> {
    if (event.key !== "ArrowUp" && event.key !== "ArrowDown") return;
    event.preventDefault();
    if (disposed || orderSaving.value || busy.value || sortableLength() < 2) return;
    const items = currentGroups();
    const fromIndex = items.findIndex((item) => item.id === accountId);
    const toIndex = fromIndex + (event.key === "ArrowUp" ? -1 : 1);
    if (fromIndex < 0 || toIndex < 0 || toIndex >= items.length) return;
    const source = items[fromIndex];
    const target = items[toIndex];
    if ((source?.credentials.length ?? 0) === 0) return;
    if ((target?.credentials.length ?? 0) === 0) return;
    const previous = [...displayedAccounts.value];
    applyGroups(moveItem(items, fromIndex, toIndex), previous);
    await persistAccountOrder(previous, accountId);
  }

  async function persistExplicitOrder(nextIds: string[]): Promise<void> {
    if (disposed || orderSaving.value || busy.value) return;
    const previous = [...displayedAccounts.value];
    const byId = new Map(previous.map((account) => [account.id, account]));
    if (nextIds.length !== previous.length || nextIds.some((id) => !byId.has(id))) return;
    preview(nextIds.map((id) => byId.get(id)!));
    await persistAccountOrder(previous, nextIds[0] ?? "");
  }

  /** Restore the pre-drag order and detach listeners; used on view unmount. */
  function revertActiveDrag(): void {
    disposed = true;
    previewOrder.value = null;
    if (accountDrag) {
      clearAccountDrag(accountDrag);
    }
  }

  return {
    orderSaving,
    draggingAccountId,
    orderAnnouncement,
    startAccountDrag,
    handleOrderKeydown,
    persistExplicitOrder,
    revertActiveDrag,
  };
}
