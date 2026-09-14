import type { ProviderPreset } from "./provider-presets.ts";

/**
 * Vendor family metadata for the chooser rail and the Providers page left
 * pane. The frozen table is the single source of truth: label is the
 * user-facing brand name and `tint` is the monogram fallback background
 * color. Brand logo SVGs resolve at the component layer
 * (`src/components/provider-brand-logos.ts`) so this module stays runnable
 * under raw `node --test`. Tints are brand-asset colors and intentionally
 * do not flow through the design tokens.
 */

export interface ProviderFamily {
  id: string;
  label: string;
  /** Hex background color for the monogram fallback (`#RRGGBB`). */
  tint: string;
}

/**
 * Static brand metadata. Frozen so callers can rely on identity. Tints are
 * chosen to be recognizable per brand and to remain readable against the
 * dashboard's neutral surface. OpenAI, Microsoft, AWS, xAI and the small
 * Chinese vendors that have no simple-icons entry fall back to a monogram.
 */
const FAMILIES: readonly ProviderFamily[] = Object.freeze([
  { id: "openai", label: "OpenAI", tint: "#10A37F" },
  { id: "anthropic", label: "Anthropic", tint: "#D97757" },
  { id: "google", label: "Google", tint: "#8E75B2" },
  { id: "xai", label: "xAI", tint: "#1A1A1A" },
  { id: "microsoft", label: "Microsoft Azure", tint: "#0078D4" },
  { id: "aws", label: "AWS", tint: "#FF9900" },
  { id: "deepseek", label: "DeepSeek", tint: "#5786FE" },
  { id: "moonshot", label: "Moonshot / Kimi", tint: "#1F1F1F" },
  { id: "zhipu", label: "Zhipu AI", tint: "#3B5BFD" },
  { id: "minimax", label: "MiniMax", tint: "#FF5A1F" },
  { id: "longcat", label: "LongCat", tint: "#FF6B35" },
  { id: "tencent", label: "Tencent", tint: "#0052D9" },
  { id: "alibaba", label: "Alibaba Cloud", tint: "#FF6A00" },
  { id: "bytedance", label: "Volcengine / BytePlus", tint: "#3C8CFF" },
  { id: "baidu", label: "Baidu Qianfan", tint: "#2932E1" },
  { id: "stepfun", label: "StepFun", tint: "#5B47E0" },
  { id: "xiaomi", label: "Xiaomi MiMo", tint: "#FF5700" },
  { id: "ant-ling", label: "Ant Ling", tint: "#1677FF" },
  { id: "streamlake", label: "StreamLake", tint: "#0EA5E9" },
  { id: "openrouter", label: "OpenRouter", tint: "#5A6B82" },
  { id: "siliconflow", label: "SiliconFlow", tint: "#7E3FF2" },
  { id: "nvidia", label: "NVIDIA", tint: "#76B900" },
  { id: "modelscope", label: "ModelScope", tint: "#624AFF" },
  { id: "ppio", label: "PPIO", tint: "#1E88E5" },
  { id: "qiniu", label: "Qiniu", tint: "#07BE0F" },
  { id: "novita", label: "Novita AI", tint: "#9C27B0" },
  { id: "compshare", label: "Compshare", tint: "#2E7CF6" },
  { id: "atlascloud", label: "AtlasCloud", tint: "#3DDC97" },
  { id: "ollama", label: "Ollama", tint: "#000000" },
]);

const FAMILIES_BY_ID: ReadonlyMap<string, ProviderFamily> = new Map(
  FAMILIES.map((family) => [family.id, family]),
);

export const PROVIDER_FAMILIES: readonly ProviderFamily[] = FAMILIES;

const UNKNOWN_FAMILY_TINT = "#5F6068";

/**
 * Family lookup for a single preset. A missing or unknown `family` id falls
 * back to a synthesized single-preset family so legacy or out-of-date rows
 * still render without breaking the chooser.
 */
export function familyOf(preset: Pick<ProviderPreset, "id" | "name" | "family">): ProviderFamily {
  if (preset.family) {
    const known = FAMILIES_BY_ID.get(preset.family);
    if (known) return known;
  }
  return {
    id: preset.id,
    label: preset.name,
    tint: UNKNOWN_FAMILY_TINT,
  };
}

/**
 * Group presets by family, preserving JSON order. The first appearance of a
 * family id defines its group position; later occurrences are appended to
 * the existing group in their original order. Presets without a known family
 * are grouped under a synthesized per-preset family.
 */
export function groupPresetsByFamily(
  presets: readonly ProviderPreset[],
): { family: ProviderFamily; presets: ProviderPreset[] }[] {
  const groups: { family: ProviderFamily; presets: ProviderPreset[] }[] = [];
  const byId = new Map<string, { family: ProviderFamily; presets: ProviderPreset[] }>();
  for (const preset of presets) {
    const family = familyOf(preset);
    const existing = byId.get(family.id);
    if (existing) {
      existing.presets.push(preset);
    } else {
      const group = { family, presets: [preset] };
      byId.set(family.id, group);
      groups.push(group);
    }
  }
  return groups;
}
