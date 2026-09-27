import { computed, ref } from "vue";
import { useDialog, useMessage } from "naive-ui";
import type { Account } from "../api/dashboard.ts";
import { useAccountLifecycleStore } from "../stores/accountLifecycle.ts";
import { useBillingStore } from "../stores/billing.ts";
import { usePlatformAccountsStore } from "../stores/platformAccounts.ts";
import { t } from "../i18n/index.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";

/** Row-local confirmation; persisted state and duplicate suppression stay in Pinia. */
export function useAccountRemoval() {
  const lifecycle = useAccountLifecycleStore();
  const billing = useBillingStore();
  const platforms = usePlatformAccountsStore();
  const dialog = useDialog();
  const message = useMessage();
  const confirming = ref(false);

  function confirmDelete(account: Account): void {
    if (confirming.value || lifecycle.deleting[account.id] || platforms.mutating) return;
    confirming.value = true;
    const session = billing.sessionEpoch;
    dialog.warning({
      title: t("删除账号"),
      content: t("删除账号 {name}？账号数据、独立浏览器中的 Cookie 和 Profile 都会被删除。", { name: account.name }),
      positiveText: t("删除"),
      negativeText: t("取消"),
      onAfterLeave: () => { confirming.value = false; },
      onPositiveClick: async () => {
        if (session !== billing.sessionEpoch) return true;
        try {
          const outcome = await lifecycle.remove(account.id);
          if (session !== billing.sessionEpoch || outcome.kind === "cancelled") return true;
          if (outcome.kind === "conflict") {
            message.warning(t("账号设置已被其他操作修改，已重新加载最新状态，请重试"));
          } else {
            message.success(t("账号已删除"));
            if (outcome.kind === "deleted_refresh_failed") {
              message.warning(t("加载账号失败：{error}", { error: outcome.detail }));
            }
          }
          return true;
        } catch (error) {
          if (session !== billing.sessionEpoch) return true;
          message.error(t("删除失败：{error}", { error: dashboardErrorDetail(error) }));
          return false;
        }
      },
    });
  }

  return { confirmDelete, deleting: computed(() => lifecycle.deleting) };
}
