import type {
  Destination,
  DestinationModelMetadataSnapshot,
} from "../api/destinations.ts";
import type { MessageKey } from "../i18n/index.ts";
import { CPA_PROVIDER_ID } from "./destination-providers.ts";
import type { ProviderAliasRow } from "./provider-aliases.ts";

/**
 * Alias-table capability projection. Declarations stay route-scoped on the
 * Providers page; this module only reads the per-destination effective
 * metadata and joins it onto alias rows so each mapping shows what the route
 * behind it can actually do. CPA rows own no destination metadata and are
 * reported as unavailable instead of guessing.
 */
export type AliasCapabilityState = "pending" | "error" | "unavailable" | "unknown" | "ready";

export interface AliasCapabilityView {
  state: AliasCapabilityState;
  /** The metadata-owning destination, when the row kind has one. */
  destination_id: string | null;
  /** Raw server provenance (`operator` | `upstream` | `modelsdev`) when ready. */
  source: string | null;
  input_modalities: readonly string[];
  output_modalities: readonly string[];
}

/** Provenance labels; unknown codes render the raw string. */
export const ALIAS_CAPABILITY_SOURCE_KEYS: Record<string, MessageKey> = {
  operator: "人工声明",
  upstream: "上游发现",
  modelsdev: "models.dev 目录",
};

export const ALIAS_CAPABILITY_STATE_KEYS: Record<
  Exclude<AliasCapabilityState, "ready">,
  MessageKey
> = {
  pending: "加载中…",
  error: "加载失败",
  unavailable: "未知",
  unknown: "未知",
};

export const ALIAS_MODALITY_KEYS: Record<string, MessageKey> = {
  text: "文本",
  image: "图片",
  audio: "音频",
  video: "视频",
};

/**
 * The destination whose model-metadata endpoint describes this row, matching
 * the resolutions ModelMetadataEditor already uses: custom accounts join by
 * account id, provider scopes by builtin/dynamic legacy id. CPA rows have no
 * metadata-owning destination and resolve to null.
 */
export function aliasRowDestinationId(
  row: ProviderAliasRow,
  destinations: readonly Destination[],
): string | null {
  if (row.custom_account_id) {
    return destinations.find((destination) => (
      destination.legacy.kind === "custom_account" && destination.legacy.id === row.custom_account_id
    ))?.id ?? null;
  }
  if (!row.provider_id || row.provider_id === CPA_PROVIDER_ID) return null;
  return destinations.find((destination) => (
    (destination.legacy.kind === "builtin" || destination.legacy.kind === "dynamic")
      && destination.legacy.id === row.provider_id
  ))?.id ?? null;
}

/**
 * Project one row's effective capabilities. Modalities are the display fact:
 * a route whose source is known but whose modalities nobody reported still
 * renders as unknown, because per-field fallback may leave gaps.
 */
export function aliasCapabilityView(
  row: ProviderAliasRow,
  destinations: readonly Destination[],
  metadata: Readonly<Record<string, DestinationModelMetadataSnapshot | undefined>>,
  errors: Readonly<Record<string, string | undefined>>,
): AliasCapabilityView {
  const destinationId = aliasRowDestinationId(row, destinations);
  const empty = { input_modalities: [], output_modalities: [] };
  if (!destinationId) {
    return { state: "unavailable", destination_id: null, source: null, ...empty };
  }
  const snapshot = metadata[destinationId];
  if (!snapshot) {
    return {
      state: errors[destinationId] ? "error" : "pending",
      destination_id: destinationId,
      source: null,
      ...empty,
    };
  }
  // Table identity is the public model on HTTP scopes and the upstream ID on
  // builtin scopes; mirror the editor's lookup order.
  const entry = snapshot.models.find((model) => model.public_model === row.public_model)
    ?? snapshot.models.find((model) => model.upstream_model === row.upstream_model);
  if (!entry || !entry.metadata.input_modalities) {
    return { state: "unknown", destination_id: destinationId, source: entry?.source ?? null, ...empty };
  }
  return {
    state: "ready",
    destination_id: destinationId,
    source: entry.source,
    input_modalities: entry.metadata.input_modalities,
    output_modalities: entry.metadata.output_modalities ?? [],
  };
}
