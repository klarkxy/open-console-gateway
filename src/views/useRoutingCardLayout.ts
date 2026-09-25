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
    /** Live preview order, moved by pointer events without touching reactive state. */
    previewCardIds: string[];
    previewCredentialIds: string[] | null;
    cardsContainer: HTMLElement | null;
    rowsContainer: HTMLElement | null;
  } | null = null;

  let previewFrame: number | null = null;
  let previewEvent: PointerEvent | null = null;

  function dropPendingPreview() {
    if (previewFrame !== null) { cancelAnimationFrame(previewFrame); previewFrame = null; }
    previewEvent = null;
  }

  /** Physically reorder the wrapper elements to `ids`; Vue state is untouched. */
  function applyDomOrder(container: HTMLElement | null, attr: string, ids: readonly string[]): void {
    if (!container) return;
    const wrappers = new Map<string, HTMLElement>();
    for (const el of Array.from(container.querySelectorAll<HTMLElement>(`[${attr}]`))) {
      const id = el.getAttribute(attr);
      if (id) wrappers.set(id, el);
    }
    if (wrappers.size !== ids.length) return;
    let anchor: Node | null = null;
    for (let index = ids.length - 1; index >= 0; index--) {
      const el = wrappers.get(ids[index]!);
      if (!el) return;
      if (el.nextSibling !== anchor) container.insertBefore(el, anchor);
      anchor = el;
    }
  }

  /** Undo the manual DOM preview so the physical order matches the last Vue render again. */
  function restoreDomOrder(active: NonNullable<typeof drag>): void {
    if (active.credentialId) {
      const card = active.previous.find((item) => item.id === active.cardId);
      if (card) applyDomOrder(active.rowsContainer, "data-layout-row-id", card.credentialIds);
    } else {
      applyDomOrder(active.cardsContainer, "data-layout-card-id", active.previous.map((card) => card.id));
    }
  }

  function clearDrag() {
    const old = drag;
    drag = null;
    draggingCardId.value = null;
    draggingCredentialId.value = null;
    dropPendingPreview();
    window.removeEventListener("pointermove", previewDrag);
    window.removeEventListener("pointerup", finishDrag);
    window.removeEventListener("pointercancel", cancelArrangement);
    if (old?.handle.hasPointerCapture(old.pointerId)) old.handle.releasePointerCapture(old.pointerId);
  }
  function cancelArrangement() {
    options.draft.value = null;
    const old = drag;
    if (old) {
      clearDrag();
      // The sync revision/busy watcher runs before the cancelling commit's
      // render flushes, so restoring here keeps the DOM at the order Vue last
      // rendered and the patch stays consistent.
      restoreDomOrder(old);
    }
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
    const layout = activeLayout.value;
    drag = {
      cardId,
      credentialId,
      handle,
      pointerId: event.pointerId,
      revision: { ...options.revision.value },
      previous: clone(layout),
      previewCardIds: layout.map((card) => card.id),
      previewCredentialIds: credentialId
        ? [...(layout.find((card) => card.id === cardId)?.credentialIds ?? [])]
        : null,
      cardsContainer: handle.closest<HTMLElement>(".account-list"),
      rowsContainer: credentialId ? handle.closest<HTMLElement>(".destination-rows") : null,
    };
    if (credentialId) draggingCredentialId.value = credentialId;
    else draggingCardId.value = cardId;
    window.addEventListener("pointermove", previewDrag, { passive: false });
    window.addEventListener("pointerup", finishDrag);
    window.addEventListener("pointercancel", cancelArrangement);
  }
  function previewDrag(event: PointerEvent) {
    if (!drag || drag.pointerId !== event.pointerId) return;
    event.preventDefault();
    previewEvent = event;
    if (previewFrame === null) previewFrame = requestAnimationFrame(applyPendingPreview);
  }
  /**
   * Reorder the preview from the latest throttled pointer event; boundary
   * crossings only. The preview moves DOM nodes directly and keeps the order
   * in plain drag state, so a mid-drag frame costs one insertBefore instead
   * of a reactive draft commit that re-renders the whole list.
   */
  function applyPendingPreview() {
    if (previewFrame !== null) { cancelAnimationFrame(previewFrame); previewFrame = null; }
    const event = previewEvent;
    previewEvent = null;
    if (!drag || !event) return;
    const target = document.elementFromPoint(event.clientX, event.clientY);
    const targetCard = target?.closest<HTMLElement>(".account-card[data-account-id]")?.dataset.accountId;
    if (drag.credentialId) {
      if (targetCard !== drag.cardId) return;
      const targetRow = target?.closest<HTMLElement>(".credential-row[data-credential-id]")?.dataset.credentialId;
      const ids = drag.previewCredentialIds;
      if (!ids || !targetRow) return;
      const from = ids.indexOf(drag.credentialId);
      const to = ids.indexOf(targetRow);
      if (from < 0 || to < 0 || from === to) return;
      drag.previewCredentialIds = moveItem(ids, from, to);
      applyDomOrder(drag.rowsContainer, "data-layout-row-id", drag.previewCredentialIds);
    } else {
      const ids = drag.previewCardIds;
      const from = ids.indexOf(drag.cardId);
      const to = targetCard ? ids.indexOf(targetCard) : -1;
      if (from < 0 || to < 0 || from === to) return;
      drag.previewCardIds = moveItem(ids, from, to);
      applyDomOrder(drag.cardsContainer, "data-layout-card-id", drag.previewCardIds);
    }
  }
  async function finishDrag(event: PointerEvent) {
    if (!drag || event.pointerId !== drag.pointerId) return;
    applyPendingPreview();
    const old = drag;
    const byId = new Map(old.previous.map((card) => [card.id, card] as const));
    // The drop commits the previewed order once; the render then finds the DOM
    // already in that order and only reconciles props.
    const next = old.credentialId
      ? old.previous.map((card) => (card.id === old.cardId
        ? { ...card, credentialIds: [...(old.previewCredentialIds ?? card.credentialIds)] }
        : card))
      : old.previewCardIds
        .map((id) => byId.get(id))
        .filter((card): card is RoutingCardLayoutDraft => Boolean(card));
    clearDrag();
    if (next.length !== old.previous.length || JSON.stringify(old.previous) === JSON.stringify(next)) {
      restoreDomOrder(old);
      return;
    }
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
