import type { PricingAdjustment, PricingModel } from "../api/dashboard";

export interface FormattedPricingRate {
  label: string;
  exact: string | null;
}

export interface PricingTableLabels {
  highspeed: string;
  minimaxM3Upper: string;
  priorityService: string;
  minimaxM3UpperPriority: string;
}

export interface PricingTableRow {
  row_key: string;
  kind: "model" | "group" | "variant";
  model_id: string;
  display_name: string;
  input: number | null | undefined;
  output: number | null | undefined;
  cache_read: number | null | undefined;
  cache_write: number | null | undefined;
  usage: number | null | undefined;
  quota_multiplier: number;
  editable_multiplier: boolean;
  children?: PricingTableRow[];
}

/**
 * Pricing rates are shown compactly in the table, while tiny non-zero values
 * retain an exact, inspectable representation for the tooltip.
 */
export function formatPricingRate(
  value: number | null,
  locale: string,
): FormattedPricingRate {
  if (value === null || !Number.isFinite(value)) return { label: "—", exact: null };
  if (value !== 0 && Math.abs(value) < 0.01) {
    const exact = new Intl.NumberFormat(locale, {
      style: "currency",
      currency: "USD",
      currencyDisplay: "narrowSymbol",
      minimumFractionDigits: 2,
      maximumFractionDigits: 8,
    }).format(value);
    return { label: value > 0 ? "<$0.01" : ">-$0.01", exact };
  }
  return {
    label: new Intl.NumberFormat(locale, {
      style: "currency",
      currency: "USD",
      currencyDisplay: "narrowSymbol",
      minimumFractionDigits: 2,
      maximumFractionDigits: 2,
    }).format(value),
    exact: null,
  };
}

export function formatPricingMultiplier(value: number): string {
  return `×${new Intl.NumberFormat("en-US", { maximumFractionDigits: 4 }).format(value)}`;
}

function variantLabel(displayName: string): string {
  return displayName.match(/\(([^()]*)\)\s*$/)?.[1]?.trim() || displayName;
}

function baseDisplayName(displayName: string): string {
  return displayName.replace(/\s*\([^()]*\)\s*$/, "").trim();
}

function scaleRate(value: number | null, multiplier: number): number | null {
  return value === null ? null : value * multiplier;
}

function modelRow(
  model: PricingModel,
  rowKey: string,
  kind: PricingTableRow["kind"] = "model",
  displayName = model.display_name || model.model_id,
): PricingTableRow {
  return {
    row_key: rowKey,
    kind,
    model_id: model.model_id,
    display_name: displayName,
    input: model.input,
    output: model.output,
    cache_read: model.cache_read,
    cache_write: model.cache_write,
    usage: model.usage,
    quota_multiplier: model.quota_multiplier,
    editable_multiplier: kind === "model",
  };
}

function groupRow(model: PricingModel, children: PricingTableRow[]): PricingTableRow {
  return {
    row_key: `group:${model.model_id}`,
    kind: "group",
    model_id: model.model_id,
    display_name: baseDisplayName(model.display_name || model.model_id),
    input: model.input,
    output: model.output,
    cache_read: model.cache_read,
    cache_write: model.cache_write,
    usage: model.usage,
    quota_multiplier: model.quota_multiplier,
    editable_multiplier: true,
    children,
  };
}

function materializedVariant(
  model: PricingModel,
  adjustment: PricingAdjustment,
  rowKey: string,
  displayName: string,
): PricingTableRow {
  const adjustedFields = new Set(adjustment.applies_to.split(",").map((field) => field.trim()));
  const adjustedRate = (field: string, value: number | null): number | null => (
    adjustedFields.has(field) ? scaleRate(value, adjustment.multiplier) : value
  );
  return modelRow({
    ...model,
    input: adjustedRate("input", model.input) ?? model.input,
    output: adjustedRate("output", model.output) ?? model.output,
    cache_read: adjustedRate("cache_read", model.cache_read),
    cache_write: adjustedRate("cache_write", model.cache_write),
  }, rowKey, "variant", displayName);
}

function adjustmentLabel(adjustment: PricingAdjustment, labels: PricingTableLabels): string {
  switch (adjustment.label.trim().toLowerCase()) {
    case "highspeed alias": return labels.highspeed;
    case ">512k input": return labels.minimaxM3Upper;
    case "priority service tier": return labels.priorityService;
    case ">512k + priority": return labels.minimaxM3UpperPriority;
    default: return adjustment.label;
  }
}

function timeWindowRank(model: PricingModel): number {
  switch (model.time_window) {
    case "off_peak":
      return 0;
    case "peak":
      return 2;
    default:
      return 1;
  }
}

function comparePricingTiers(left: PricingModel, right: PricingModel): number {
  return (left.min_input_tokens ?? 0) - (right.min_input_tokens ?? 0)
    || (left.max_input_tokens ?? Number.MAX_SAFE_INTEGER)
      - (right.max_input_tokens ?? Number.MAX_SAFE_INTEGER)
    || timeWindowRank(left) - timeWindowRank(right);
}

function timeWindowKey(model: PricingModel): string {
  return model.time_window ?? "always";
}

function tierLabel(model: PricingModel): string {
  const explicit = variantLabel(model.display_name || model.model_id);
  if (explicit !== (model.display_name || model.model_id)) return explicit;
  const bounds = model.min_input_tokens != null || model.max_input_tokens != null
    ? `${model.min_input_tokens ?? 0}–${model.max_input_tokens ?? "∞"} input tokens`
    : "";
  const time = model.time_window && model.time_window !== "always"
    ? model.time_window.replaceAll("_", " ")
    : "";
  return [bounds, time].filter(Boolean).join(" · ") || explicit;
}

function adjustmentRows(
  model: PricingModel,
  labels: PricingTableLabels,
  prefix: string,
): PricingTableRow[] {
  return model.adjustments.map((adjustment, index) => materializedVariant(
    model,
    adjustment,
    `${prefix}:adjustment:${index}:${adjustment.label}`,
    adjustmentLabel(adjustment, labels),
  ));
}

/**
 * Converts flat API pricing into tree rows without changing the model IDs that
 * clients submit. A grouped parent is the standard tier with full prices;
 * only upgrade tiers are nested beneath it.
 */
export function buildPricingTableRows(
  models: readonly PricingModel[],
  labels: PricingTableLabels,
): PricingTableRow[] {
  const modelsById = new Map<string, PricingModel[]>();
  for (const model of models) {
    const entries = modelsById.get(model.model_id) ?? [];
    entries.push(model);
    modelsById.set(model.model_id, entries);
  }

  const rows: PricingTableRow[] = [];
  for (const [modelId, entries] of modelsById) {
    const first = entries[0];
    if (!first) continue;
    if (entries.length > 1) {
      const [standard, ...upgrades] = [...entries].sort(comparePricingTiers);
      if (!standard) continue;
      const children = [
        ...adjustmentRows(standard, labels, `variant:${modelId}:base`),
        ...upgrades.flatMap((entry, index) => {
          const key = `variant:${modelId}:${entry.min_input_tokens ?? "none"}:${entry.max_input_tokens ?? "none"}:${timeWindowKey(entry)}:${index}`;
          return [
            modelRow(entry, key, "variant", tierLabel(entry)),
            ...adjustmentRows(entry, labels, key),
          ];
        }),
      ];
      rows.push(groupRow(standard, children));
      continue;
    }
    const adjustments = adjustmentRows(first, labels, `variant:${modelId}:base`);
    if (adjustments.length > 0) {
      rows.push(groupRow(first, adjustments));
      continue;
    }
    rows.push(modelRow(
      first,
      `model:${modelId}:${first.min_input_tokens ?? "none"}:${first.max_input_tokens ?? "none"}:${timeWindowKey(first)}`,
    ));
  }
  return rows;
}
