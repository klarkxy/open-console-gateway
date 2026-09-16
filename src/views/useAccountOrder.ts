import { ref } from "vue";
import type { Ref } from "vue";
import { useMessage } from "naive-ui";
import { DashboardRequestError, dashboardApi } from "../api/dashboard";
import type { Account } from "../api/dashboard";
import { moveItem } from "../domain/account-lifecycle.ts";
import {
  expandAccountRouteOrder,
  type AccountRouteItem,
} from "../domain/platform-accounts.ts";
import { t } from "../i18n/index.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";

type AccountDragState = {
  accountId: string;
  handle: HTMLElement;
  moved: boolean;
  pointerId: number;
  previous: Account[];
  items: AccountRouteItem[];
};

function sameAccountOrder(left: readonly Account[], right: readonly Account[]): boolean {
  return left.length === right.length && left.every((account, index) => account.id === right[index]?.id);
}

function accountsFromItems(
  items: readonly AccountRouteItem[],
  previous: readonly Account[],
): Account[] {
  const expanded = expandAccountRouteOrder(items);
  const byId = new Map(previous.map((account) => [account.id, account]));
  const next = expanded
    .map((id) => byId.get(id))
    .filter((account): account is Account => Boolean(account));
  const seen = new Set(next.map((account) => account.id));
  return [...next, ...previous.filter((account) => !seen.has(account.id))];
}

/**
 * Pointer-drag and keyboard reordering for the ordered account card list,
 * including persistence, screen-reader announcements, and 409 conflict
 * reconciliation against the server list.
 */
export function useAccountOrder(options: {
  accounts: Ref<Account[]>;
  busy: Ref<boolean>;
  reloadAfterRevisionConflict: () => Promise<void>;
  routeItems?: Ref<AccountRouteItem[]>;
}) {
  const {
    accounts,
    busy,
    reloadAfterRevisionConflict,
    routeItems,
  } = options;
  const message = useMessage();

  const orderSaving = ref(false);
  const draggingAccountId = ref<string | null>(null);
  const orderAnnouncement = ref("");
  let accountDrag: AccountDragState | null = null;

  function currentItems(): AccountRouteItem[] {
    return routeItems?.value ?? accounts.value.map((account) => ({
      type: "account" as const,
      id: account.id,
      account,
    }));
  }

  function applyItems(items: readonly AccountRouteItem[], previous: readonly Account[]): void {
    accounts.value = accountsFromItems(items, previous);
  }

  function sortableLength(): number {
    return currentItems().filter((item) => item.type === "account" || item.keys.length > 0).length;
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
    if (sameAccountOrder(previous, accounts.value)) return;
    orderSaving.value = true;
    try {
      const saved = await dashboardApi.reorderAccounts(accounts.value.map(({ id }) => id));
      accounts.value = saved;
      const items = currentItems();
      const moved = items.find((item) => item.id === movedItemId);
      const position = items.findIndex((item) => item.id === movedItemId) + 1;
      const name = moved?.type === "platform" ? moved.parent.name : moved?.account.name;
      if (name && position > 0) {
        orderAnnouncement.value = t("账号 {name} 已移至第 {position} 位", {
          name,
          position,
        });
      }
      message.success(t("账号顺序已更新"));
    } catch (error) {
      if (error instanceof DashboardRequestError && error.status === 409) {
        accounts.value = previous;
        try {
          await reloadAfterRevisionConflict();
        } catch {
          // The optimistic preview was already reverted; the error below asks
          // the user to retry after an explicit refresh if reconciliation fails.
        }
        const conflict = t("账号设置已被其他操作修改，已重新加载最新状态，请重试");
        orderAnnouncement.value = conflict;
        message.warning(conflict);
        return;
      } else {
        accounts.value = previous;
      }
      const failure = t("保存账号顺序失败: {error}", { error: dashboardErrorDetail(error) });
      orderAnnouncement.value = failure;
      message.error(failure);
    } finally {
      orderSaving.value = false;
    }
  }

  function startAccountDrag(event: PointerEvent, accountId: string): void {
    if (
      orderSaving.value
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
      previous: [...accounts.value],
      items: currentItems(),
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
    const targetId = target?.dataset.accountId;
    if (!targetId || targetId === state.accountId) return;
    const fromIndex = state.items.findIndex((item) => item.id === state.accountId);
    const toIndex = state.items.findIndex((item) => item.id === targetId);
    if (fromIndex < 0 || toIndex < 0 || fromIndex === toIndex) return;
    const targetItem = state.items[toIndex];
    if (targetItem?.type === "platform" && targetItem.keys.length === 0) return;
    const sourceItem = state.items[fromIndex];
    if (sourceItem?.type === "platform" && sourceItem.keys.length === 0) return;
    state.items = moveItem(state.items, fromIndex, toIndex);
    applyItems(state.items, state.previous);
    state.moved = true;
  }

  async function finishAccountDrag(event: PointerEvent): Promise<void> {
    const state = accountDrag;
    if (!state || state.pointerId !== event.pointerId) return;
    event.preventDefault();
    clearAccountDrag(state);
    if (!state.moved || sameAccountOrder(state.previous, accounts.value)) return;
    await persistAccountOrder(state.previous, state.accountId);
  }

  function cancelAccountDrag(event: PointerEvent): void {
    const state = accountDrag;
    if (!state || state.pointerId !== event.pointerId) return;
    event.preventDefault();
    accounts.value = state.previous;
    clearAccountDrag(state);
  }

  async function handleOrderKeydown(event: KeyboardEvent, accountId: string): Promise<void> {
    if (event.key !== "ArrowUp" && event.key !== "ArrowDown") return;
    event.preventDefault();
    if (orderSaving.value || busy.value || sortableLength() < 2) return;
    const items = currentItems();
    const fromIndex = items.findIndex((item) => item.id === accountId);
    const toIndex = fromIndex + (event.key === "ArrowUp" ? -1 : 1);
    if (fromIndex < 0 || toIndex < 0 || toIndex >= items.length) return;
    const source = items[fromIndex];
    const target = items[toIndex];
    if (source?.type === "platform" && source.keys.length === 0) return;
    if (target?.type === "platform" && target.keys.length === 0) return;
    const previous = [...accounts.value];
    applyItems(moveItem(items, fromIndex, toIndex), previous);
    await persistAccountOrder(previous, accountId);
  }

  async function persistExplicitOrder(nextIds: string[]): Promise<void> {
    const previous = [...accounts.value];
    const byId = new Map(previous.map((account) => [account.id, account]));
    if (nextIds.length !== previous.length || nextIds.some((id) => !byId.has(id))) return;
    accounts.value = nextIds.map((id) => byId.get(id)!);
    await persistAccountOrder(previous, nextIds[0] ?? "");
  }

  /** Restore the pre-drag order and detach listeners; used on view unmount. */
  function revertActiveDrag(): void {
    if (accountDrag) {
      accounts.value = accountDrag.previous;
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
