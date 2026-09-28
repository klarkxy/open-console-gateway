import type { ModelMetadata } from "../api/generated/dashboard-v4.ts";
import type { DestinationModelMetadataEntryView, ModelMetadataView } from "../api/destinations.ts";
import type { MessageKey } from "../i18n/index.ts";

/**
 * Draft model and validation for the per-route model metadata declaration
 * form. Mirrors the server's rules (crates/ocg-core/src/model_metadata.rs):
 * omitted means unknown, never guessed; declared facts must be consistent.
 */

export const METADATA_MODALITIES = ["text", "image", "audio", "video"] as const;
export type MetadataModality = (typeof METADATA_MODALITIES)[number];

export const REASONING_EFFORT_LEVELS = ["off", "minimal", "low", "medium", "high", "xhigh", "max"] as const;
export type ReasoningEffortLevel = (typeof REASONING_EFFORT_LEVELS)[number];

/** unknown = omit the fact; yes/no = explicit boolean declaration. */
export type MetadataTriState = "unknown" | "yes" | "no";

const MAX_TOKEN_LIMIT = Number.MAX_SAFE_INTEGER;
const EFFORT_WIRE = /^[A-Za-z0-9_-]{1,32}$/;

/** C0 controls and DEL are rejected, matching char::is_control on the server. */
function hasControlChars(value: string): boolean {
  for (const ch of value) {
    const code = ch.codePointAt(0) ?? 0;
    if (code <= 0x1f || code === 0x7f) return true;
  }
  return false;
}

export interface ModelMetadataDraft {
  /** Empty string omits the display name. */
  name: string;
  contextWindow: number | null;
  maxOutputTokens: number | null;
  /** Empty array omits the fact (unknown). */
  inputModalities: MetadataModality[];
  outputModalities: MetadataModality[];
  reasoning: MetadataTriState;
  /** Only enabled levels appear; each value is the exact wire spelling. */
  reasoningEfforts: Partial<Record<ReasoningEffortLevel, string>>;
  toolCalling: MetadataTriState;
  parallelToolCalls: MetadataTriState;
}

export type ModelMetadataIssue =
  | "invalid_name"
  | "invalid_context_window"
  | "invalid_max_output"
  | "output_exceeds_context"
  | "invalid_effort_spelling"
  | "efforts_without_reasoning"
  | "parallel_without_tool_calling";

export const MODEL_METADATA_ISSUE_KEYS = {
  invalid_name: "显示名称不能为空、不能超过 200 个字符，也不能包含控制字符",
  invalid_context_window: "上下文窗口必须是正整数",
  invalid_max_output: "最大输出必须是正整数",
  output_exceeds_context: "最大输出不能大于上下文窗口",
  invalid_effort_spelling: "档位参数只能包含字母、数字、下划线和连字符，且不超过 32 个字符",
  efforts_without_reasoning: "声明不支持推理的模型不能提供推理档位",
  parallel_without_tool_calling: "并行工具调用要求先声明支持工具调用",
} as const satisfies Record<ModelMetadataIssue, MessageKey>;

export type ModelMetadataBuild =
  | { kind: "invalid"; issue: ModelMetadataIssue }
  | { kind: "save"; metadata: ModelMetadata };

function triState(value: boolean | null): MetadataTriState {
  return value === null ? "unknown" : value ? "yes" : "no";
}

function booleanFromTriState(value: MetadataTriState): boolean | undefined {
  return value === "unknown" ? undefined : value === "yes";
}

function isTokenLimit(value: number | null): value is number {
  return value !== null && Number.isSafeInteger(value) && value > 0 && value <= MAX_TOKEN_LIMIT;
}

function isModalities(value: string[] | null): value is MetadataModality[] {
  return value !== null
    && value.length > 0
    && new Set(value).size === value.length
    && value.every((item): item is MetadataModality => (METADATA_MODALITIES as readonly string[]).includes(item));
}

/** Build the editable draft from the effective view; unknown facts start empty. */
export function modelMetadataDraft(view: ModelMetadataView): ModelMetadataDraft {
  const efforts: Partial<Record<ReasoningEffortLevel, string>> = {};
  if (view.reasoning_efforts) {
    for (const level of REASONING_EFFORT_LEVELS) {
      const wire = view.reasoning_efforts[level];
      if (wire !== undefined) efforts[level] = wire;
    }
  }
  return {
    name: view.name ?? "",
    contextWindow: view.context_window,
    maxOutputTokens: view.max_output_tokens,
    inputModalities: isModalities(view.input_modalities) ? [...view.input_modalities] : [],
    outputModalities: isModalities(view.output_modalities) ? [...view.output_modalities] : [],
    reasoning: triState(view.reasoning),
    reasoningEfforts: efforts,
    toolCalling: triState(view.tool_calling),
    parallelToolCalls: triState(view.parallel_tool_calls),
  };
}

/**
 * Validate the draft and produce the full-replacement declaration body.
 * Empty optional fields are omitted, which the server reads as unknown.
 */
export function buildModelMetadata(draft: ModelMetadataDraft): ModelMetadataBuild {
  const name = draft.name.trim();
  if (draft.name !== "" && (name === "" || name.length > 200 || hasControlChars(name))) {
    return { kind: "invalid", issue: "invalid_name" };
  }
  if (draft.contextWindow !== null && !isTokenLimit(draft.contextWindow)) {
    return { kind: "invalid", issue: "invalid_context_window" };
  }
  if (draft.maxOutputTokens !== null && !isTokenLimit(draft.maxOutputTokens)) {
    return { kind: "invalid", issue: "invalid_max_output" };
  }
  if (isTokenLimit(draft.contextWindow) && isTokenLimit(draft.maxOutputTokens)
    && draft.maxOutputTokens > draft.contextWindow) {
    return { kind: "invalid", issue: "output_exceeds_context" };
  }
  const effortEntries = REASONING_EFFORT_LEVELS
    .filter((level) => draft.reasoningEfforts[level] !== undefined)
    .map((level) => [level, draft.reasoningEfforts[level]!] as const);
  if (effortEntries.some(([, wire]) => !EFFORT_WIRE.test(wire))) {
    return { kind: "invalid", issue: "invalid_effort_spelling" };
  }
  if (draft.reasoning === "no" && effortEntries.length > 0) {
    return { kind: "invalid", issue: "efforts_without_reasoning" };
  }
  if (draft.toolCalling === "no" && draft.parallelToolCalls === "yes") {
    return { kind: "invalid", issue: "parallel_without_tool_calling" };
  }
  const reasoning = booleanFromTriState(draft.reasoning);
  const toolCalling = booleanFromTriState(draft.toolCalling);
  const parallelToolCalls = booleanFromTriState(draft.parallelToolCalls);
  return {
    kind: "save",
    metadata: {
      ...(name !== "" ? { name } : {}),
      ...(isTokenLimit(draft.contextWindow) ? { contextWindow: draft.contextWindow } : {}),
      ...(isTokenLimit(draft.maxOutputTokens) ? { maxOutputTokens: draft.maxOutputTokens } : {}),
      ...(draft.inputModalities.length > 0 ? { inputModalities: [...draft.inputModalities] } : {}),
      ...(draft.outputModalities.length > 0 ? { outputModalities: [...draft.outputModalities] } : {}),
      ...(reasoning !== undefined ? { reasoning } : {}),
      ...(effortEntries.length > 0
        ? { reasoningEfforts: Object.fromEntries(effortEntries) }
        : {}),
      ...(toolCalling !== undefined ? { toolCalling } : {}),
      ...(parallelToolCalls !== undefined ? { parallelToolCalls } : {}),
    },
  };
}

/**
 * Stable fingerprint of the effective entry for staleness checks: any change
 * to the stored facts or their source invalidates a captured form baseline.
 */
export function modelMetadataFingerprint(
  entry: Pick<DestinationModelMetadataEntryView, "metadata" | "source"> | null,
): string {
  return JSON.stringify(entry ?? null);
}
