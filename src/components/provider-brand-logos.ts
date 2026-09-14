/**
 * Brand logo asset lookup. This module is component-layer only: the literal
 * `import.meta.glob` call is rewritten by Vite, so it must never be imported
 * from `src/domain/` modules that run under raw `node --test`.
 */

const LOGO_MODULES = import.meta.glob("../assets/provider-logos/*.svg", {
  eager: true,
  query: "?url",
  import: "default",
}) as Record<string, string>;

/** Resolved asset URL for a family logo SVG, or null when none ships. */
export function providerBrandLogo(familyId: string): string | null {
  const path = `../assets/provider-logos/${familyId}.svg`;
  return Object.prototype.hasOwnProperty.call(LOGO_MODULES, path)
    ? LOGO_MODULES[path]!
    : null;
}

/**
 * Shipped SVGs whose artwork is near-black on transparency. They get a
 * neutral contrast plate per icon so they stay visible on a dark surface;
 * colored logos are never plated or inverted.
 */
const DARK_LOGO_FAMILY_IDS: ReadonlySet<string> = new Set([
  "anthropic",
  "ollama",
]);

export function providerBrandLogoNeedsPlate(familyId: string): boolean {
  return DARK_LOGO_FAMILY_IDS.has(familyId);
}
