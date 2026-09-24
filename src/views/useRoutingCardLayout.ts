import { computed, ref, watch } from "vue";
import type { Ref } from "vue";
import type { MessageApi } from "naive-ui";
import { isRevisionConflict } from "../api/dashboard.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
import { moveItem } from "../domain/account-lifecycle.ts";
import { t } from "../i18n/index.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";

export interface RoutingCardLayoutDraft {
  id: string;
  destinationId: string;
  credentialIds: string[];
}
const clone = (cards: readonly RoutingCardLayoutDraft[]) => cards.map(card => ({ ...card, credentialIds: [...card.credentialIds] }));

/** Optimistic layout only. The store owns receipts and conflict revalidation. */
export function useRoutingCardLayout(options: {
  committedLayout: Ref<RoutingCardLayoutDraft[]>;
  revision: Ref<MutationExpectation | null>;
  draft: Ref<RoutingCardLayoutDraft[] | null>;
  busy: Ref<boolean>;
  message: Pick<MessageApi, "success" | "warning" | "error">;
  refreshConflict: () => Promise<unknown>;
  save: (layout: RoutingCardLayoutDraft[], revision: MutationExpectation) => Promise<void>;
}) {
  const orderSaving = ref(false);
  const draggingCardId = ref<string | null>(null);
  const draggingCredentialId = ref<string | null>(null);
  const orderAnnouncement = ref("");
  const activeLayout = computed(() => options.draft.value ?? options.committedLayout.value);
  let disposed = false;
  let operation = 0;
  let drag: {
    cardId: string; credentialId?: string; handle: HTMLElement; pointerId: number;
    revision: MutationExpectation; previous: RoutingCardLayoutDraft[];
  } | null = null;

  function clearDrag() {
    const old = drag;
    drag = null;
    draggingCardId.value = null;
    draggingCredentialId.value = null;
    window.removeEventListener("pointermove", previewDrag);
    window.removeEventListener("pointerup", finishDrag);
    window.removeEventListener("pointercancel", cancelArrangement);
    if (old?.handle.hasPointerCapture(old.pointerId)) old.handle.releasePointerCapture(old.pointerId);
  }
  function cancelArrangement() {
    options.draft.value = null;
    if (drag) clearDrag();
  }
  const stopWatch = watch([options.revision, options.busy], ([revision, busy]) => {
    if (!revision) { operation += 1; cancelArrangement(); }
    else if (drag && (busy || revision.expectedRevision !== drag.revision.expectedRevision
      || revision.processGeneration !== drag.revision.processGeneration)) cancelArrangement();
  }, { flush: "sync" });

  async function persist(next: RoutingCardLayoutDraft[], revision: MutationExpectation): Promise<boolean> {
    const own = ++operation;
    orderSaving.value = true;
    options.draft.value = next;
    try {
      await options.save(next, revision);
      if (disposed || own !== operation || !options.revision.value) return false;
      options.draft.value = null;
      orderAnnouncement.value = t("账号顺序已更新");
      return true;
    } catch (error) {
      if (disposed || own !== operation) return false;
      options.draft.value = null;
      if (isRevisionConflict(error)) {
        try { await options.refreshConflict(); }
        catch (refreshError) {
          if (!disposed && own === operation) options.message.error(t("加载账号失败：{error}", { error: dashboardErrorDetail(refreshError) }));
          return false;
        }
        if (disposed || own !== operation) return false;
        orderAnnouncement.value = t("账号设置已被其他操作修改，已重新加载最新状态，请重试");
        options.message.warning(orderAnnouncement.value);
      } else {
        orderAnnouncement.value = t("保存账号顺序失败：{error}", { error: dashboardErrorDetail(error) });
        options.message.error(orderAnnouncement.value);
      }
      return false;
    } finally { orderSaving.value = false; }
  }
  async function applyLayoutChange(next: RoutingCardLayoutDraft[]): Promise<boolean> {
    if (disposed || orderSaving.value || drag || options.busy.value || !options.revision.value) return false;
    if (JSON.stringify(next) === JSON.stringify(options.committedLayout.value)) return true;
    return persist(clone(next), { ...options.revision.value });
  }
  function startDrag(event: PointerEvent, cardId: string, credentialId?: string) {
    if (disposed || orderSaving.value || options.busy.value || drag || !options.revision.value
      || !event.isPrimary || (event.pointerType === "mouse" && event.button !== 0)) return;
    event.preventDefault();
    const handle = event.currentTarget as HTMLElement;
    handle.setPointerCapture(event.pointerId);
    drag = { cardId, credentialId, handle, pointerId: event.pointerId, revision: { ...options.revision.value }, previous: clone(activeLayout.value) };
    if (credentialId) draggingCredentialId.value = credentialId;
    else draggingCardId.value = cardId;
    window.addEventListener("pointermove", previewDrag, { passive: false });
    window.addEventListener("pointerup", finishDrag);
    window.addEventListener("pointercancel", cancelArrangement);
  }
  function previewDrag(event: PointerEvent) {
    if (!drag || drag.pointerId !== event.pointerId) return;
    event.preventDefault();
    const target = document.elementFromPoint(event.clientX, event.clientY);
    const targetCard = target?.closest<HTMLElement>(".account-card[data-account-id]")?.dataset.accountId;
    const items = clone(activeLayout.value);
    if (drag.credentialId) {
      if (targetCard !== drag.cardId) return;
      const targetRow = target?.closest<HTMLElement>(".credential-row[data-credential-id]")?.dataset.credentialId;
      const card = items.find(card => card.id === drag!.cardId);
      if (!card || !targetRow) return;
      const from = card.credentialIds.indexOf(drag.credentialId);
      const to = card.credentialIds.indexOf(targetRow);
      if (from < 0 || to < 0 || from === to) return;
      card.credentialIds = moveItem(card.credentialIds, from, to);
      options.draft.value = items;
    } else {
      const from = items.findIndex(card => card.id === drag!.cardId);
      const to = items.findIndex(card => card.id === targetCard);
      if (from < 0 || to < 0 || from === to) return;
      options.draft.value = moveItem(items, from, to);
    }
  }
  async function finishDrag(event: PointerEvent) {
    if (!drag || event.pointerId !== drag.pointerId) return;
    const old = drag;
    const next = clone(activeLayout.value);
    clearDrag();
    if (JSON.stringify(old.previous) === JSON.stringify(next)) { options.draft.value = null; return; }
    await persist(next, old.revision);
  }
  async function handleCardKeydown(event: KeyboardEvent, cardId: string) {
    if (!["ArrowUp", "ArrowDown"].includes(event.key)) return;
    event.preventDefault();
    const items = clone(activeLayout.value);
    const from = items.findIndex(card => card.id === cardId);
    const to = from + (event.key === "ArrowUp" ? -1 : 1);
    if (from >= 0 && to >= 0 && to < items.length) await applyLayoutChange(moveItem(items, from, to));
  }
  function revertActiveArrangement() { disposed = true; operation += 1; stopWatch(); cancelArrangement(); }
  return { orderSaving, orderAnnouncement, draggingCardId, draggingCredentialId, applyLayoutChange,
    startCardDrag: (event: PointerEvent, cardId: string) => startDrag(event, cardId),
    startCredentialDrag: (event: PointerEvent, cardId: string, credentialId: string) => startDrag(event, cardId, credentialId),
    handleCardKeydown, cancelArrangement, revertActiveArrangement };
}
