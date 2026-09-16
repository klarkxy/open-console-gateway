import { h, type Ref } from "vue";
import { NButton, NIcon } from "naive-ui";
import { CheckOutlined, CopyOutlined } from "@vicons/antd";
import type { Account, ForwardLog, GatewayLog } from "../api/dashboard.ts";
import { locale, t } from "../i18n/index.ts";
import { formatCost } from "../utils/format.ts";
import { formatNativeCostEstimate, forwardLogNativeEstimate } from "../domain/native-cost.ts";
import {
  forwardLogProtocol,
  forwardLogRequestedModel,
  forwardLogResolvedAlias,
  forwardLogUpstreamModel,
} from "./forward-log-display.ts";

/**
 * Imperative naive-ui column renderers for the Logs tables (request-id cells,
 * forward-log expand detail, diagnostic dumps). The view injects its local
 * state through LogsColumnContext.
 */
export interface LogsColumnContext {
  copiedTarget: Ref<string | null>;
  copyText: (target: string, value: string, label: string) => void;
  focusRequestChain: (requestId: string) => void;
  accounts: Ref<Account[]>;
}

export function shortRequestId(requestId: string): string {
  return requestId.length <= 18 ? requestId : `${requestId.slice(0, 13)}…${requestId.slice(-4)}`;
}

export function renderRequestId(row: GatewayLog | ForwardLog, ctx: LogsColumnContext) {
  const requestId = row.request_id;
  if (!requestId) return "—";
  const target = `request-id-${row.id}`;
  return h("div", { class: "request-id-cell" }, [
    h(NButton, {
      text: true,
      type: "primary",
      class: "request-id-link",
      title: requestId,
      onClick: () => ctx.focusRequestChain(requestId),
    }, { default: () => h("code", shortRequestId(requestId)) }),
    h(NButton, {
      text: true,
      type: "primary",
      "aria-label": t("复制请求 ID"),
      onClick: () => ctx.copyText(target, requestId, t("请求 ID")),
    }, {
      icon: () => h(NIcon, { component: ctx.copiedTarget.value === target ? CheckOutlined : CopyOutlined }),
    }),
  ]);
}

function renderAliasDetail(row: ForwardLog) {
  const items = (
    [
      [t("请求模型"), forwardLogRequestedModel(row)],
      [t("解析别名"), forwardLogResolvedAlias(row)],
      [t("上游模型"), forwardLogUpstreamModel(row)],
      [t("协议"), forwardLogProtocol(row)],
    ] as Array<[string, string | null]>
  ).filter((pair): pair is [string, string] => pair[1] !== null);
  if (!items.length) return null;
  return h("section", [
    h("h4", t("模型解析")),
    h("dl", { class: "diagnostic-meta" }, items.flatMap(([label, value]) => [
      h("dt", label),
      h("dd", value),
    ])),
  ]);
}

export function renderForwardDetail(row: ForwardLog, ctx: LogsColumnContext) {
  const requestId = row.request_id;
  const requestBlock = requestId
    ? h("section", [
      h("h4", t("请求 ID")),
      h("div", { class: "request-id-cell" }, [
        h("code", requestId),
        h(NButton, {
          text: true,
          type: "primary",
          "aria-label": t("复制请求 ID"),
          onClick: () => ctx.copyText(`request-id-${row.id}`, requestId, t("请求 ID")),
        }, {
          icon: () => h(NIcon, { component: ctx.copiedTarget.value === `request-id-${row.id}` ? CheckOutlined : CopyOutlined }),
        }),
        h(NButton, {
          text: true,
          type: "primary",
          onClick: () => ctx.focusRequestChain(requestId),
        }, { default: () => t("筛选此请求") }),
      ]),
    ])
    : null;
  return h("div", { class: "diagnostic-detail" }, [
    requestBlock,
    renderAliasDetail(row),
    renderProviderCost(row, ctx),
    renderDiagnostic(row),
  ]);
}

// Provider attribution and the three cost figures are nullable server-side;
// null means "unknown" and must never render as a $0 amount.
function renderProviderCost(row: ForwardLog, ctx: LogsColumnContext) {
  const costValue = (value: number | null | undefined) => (
    value === null || value === undefined ? t("未知") : formatCost(value, 5)
  );
  const accountLabel = (id: string | null | undefined) => {
    if (!id) return t("未知");
    return ctx.accounts.value.find((account) => account.id === id)?.name ?? id;
  };
  const items: Array<[string, string]> = [
    [t("服务商"), row.provider_id ?? t("未知")],
    [t("路由账号"), accountLabel(row.route_account_id)],
    [t("凭证账号"), accountLabel(row.credential_account_id)],
    [t("原始供应商成本"), costValue(row.raw_cost_usd)],
    [t("额度扣减"), costValue(row.quota_debit)],
    [t("有效付费成本"), costValue(row.effective_paid_cost_usd)],
  ];
  // Platform-native estimate: original currency, frozen pricing provenance,
  // and the actual wallet debit stays unknown — never implied by the estimate.
  const estimate = forwardLogNativeEstimate(row);
  if (estimate) {
    items.push(
      [t("平台估算（原始货币）"), formatNativeCostEstimate(estimate, locale.value)],
      [t("计价来源（冻结）"), row.pricing_revision_id ?? t("未知")],
      [t("实际平台扣减"), t("未知")],
    );
  }
  return h("section", [
    h("h4", t("供应商与费用")),
    h("dl", { class: "diagnostic-meta" }, items.flatMap(([label, value]) => [
      h("dt", label),
      h("dd", value),
    ])),
  ]);
}

function routeLegLabel(route?: string): string {
  // Empty = a row written before the route column existed: keep the honest
  // "not recorded" marker instead of hiding the row.
  if (!route) return "—";
  if (route === "auto") return t("自动");
  if (route === "proxy") return t("代理");
  if (route === "direct") return t("直连");
  return route;
}

export function renderDiagnostic(row: GatewayLog | ForwardLog) {
  const diagnostic = row.diagnostic;
  const items = [
    [t("错误来源"), row.error_source ?? diagnostic?.error_source],
    [t("失败阶段"), row.error_stage ?? diagnostic?.error_stage],
    [t("协议路径"), diagnostic?.upstream_format
      ? `${diagnostic.client_format} → ${diagnostic.upstream_format}`
      : diagnostic?.client_format],
    [t("尝试次数"), diagnostic?.attempt ?? ("attempt" in row ? row.attempt : null)],
    [t("路由"), "route" in row ? routeLegLabel(row.route) : null],
    [t("耗时"), row.duration_ms !== null && row.duration_ms !== undefined
      ? `${row.duration_ms} ms`
      : diagnostic ? `${diagnostic.duration_ms} ms` : null],
    [t("上游响应头耗时"), diagnostic?.upstream_wait_ms !== null && diagnostic?.upstream_wait_ms !== undefined
      ? `${diagnostic.upstream_wait_ms} ms` : null],
    [t("重试动作"), diagnostic?.retry_action],
  ].filter((item) => item[1] !== null && item[1] !== undefined && item[1] !== "");
  const detailBlocks = [
    diagnostic?.upstream_headers && [t("上游 Trace ID"), diagnostic.upstream_headers],
    diagnostic?.request_summary && [t("请求结构与指纹"), {
      fingerprint: diagnostic.request_fingerprint,
      summary: diagnostic.request_summary,
    }],
    diagnostic?.upstream_error && [t("脱敏上游错误"), diagnostic.upstream_error],
  ].filter(Boolean) as Array<[string, unknown]>;
  const errorMessage = "error_message" in row ? row.error_message : row.message;
  return h("div", { class: "diagnostic-detail" }, [
    h("dl", { class: "diagnostic-meta" }, items.flatMap(([label, value]) => [
      h("dt", String(label)),
      h("dd", String(value)),
    ])),
    errorMessage ? h("section", [
      h("h4", t("错误")),
      h("pre", { class: "error-text" }, errorMessage),
    ]) : null,
    ...detailBlocks.map(([label, value]) => h("section", [
      h("h4", label),
      h("pre", { class: "diagnostic-json" }, JSON.stringify(value, null, 2)),
    ])),
  ]);
}
