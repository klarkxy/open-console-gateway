import { ref } from "vue";
import type { Ref } from "vue";
import { useMessage } from "naive-ui";
import { DashboardRequestError, dashboardApi } from "../api/dashboard";
import type { Account } from "../api/dashboard";
import { moveItem } from "../domain/account-lifecycle.ts";
import { t } from "../i18n/index.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";

type AccountDragState = {
  accountId: string;
  handle: HTMLElement;
  moved: boolean;
  pointerId: number;
  previous: Account[];
};

function sameAccountOrder(left: readonly Account[], right: readonly Account[]): boolean {
  return left.length === right.length && left.every((account, index) => account.id === right[index]?.id);
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
}) {
  const {
    accounts,
    busy,
    reloadAfterRevisionConflict,
  } = options;
  const message = useMessage();

  const orderSaving = ref(false);
  const draggingAccountId = ref<string | null>(null);
  const orderAnnouncement = ref("");
  let accountDrag: AccountDragState | null = null;

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

  async function persistAccountOrder(previous: Account[], movedAccountId: string): Promise<void> {
    if (sameAccountOrder(previous, accounts.value)) return;
    orderSaving.value = true;
    try {
      const saved = await dashboardApi.reorderAccounts(accounts.value.map(({ id }) => id));
      accounts.value = saved;
      const moved = accounts.value.find(({ id }) => id === movedAccountId);
      const position = accounts.value.findIndex(({ id }) => id === movedAccountId) + 1;
      if (moved && position > 0) {
        orderAnnouncement.value = t("账号 {name} 已移至第 {position} 位", {
          name: moved.name,
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
      || accounts.value.length < 2
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
    const fromIndex = accounts.value.findIndex(({ id }) => id === state.accountId);
    const toIndex = accounts.value.findIndex(({ id }) => id === targetId);
    if (fromIndex < 0 || toIndex < 0 || fromIndex === toIndex) return;
    accounts.value = moveItem(accounts.value, fromIndex, toIndex);
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
    if (orderSaving.value || busy.value || accounts.value.length < 2) return;
    const fromIndex = accounts.value.findIndex(({ id }) => id === accountId);
    const toIndex = fromIndex + (event.key === "ArrowUp" ? -1 : 1);
    if (fromIndex < 0 || toIndex < 0 || toIndex >= accounts.value.length) return;
    const previous = [...accounts.value];
    accounts.value = moveItem(accounts.value, fromIndex, toIndex);
    await persistAccountOrder(previous, accountId);
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
    revertActiveDrag,
  };
}
