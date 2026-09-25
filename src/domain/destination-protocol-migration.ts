import type { Destination } from "../api/destinations.ts";
import { MAX_HTTP_PROTOCOL_ROUTES } from "./destination-catalog.ts";
import { destinationDraftRoutes, type DestinationProtocolRouteDraft } from "./destination-edit.ts";
import {
  providerPresetRoutesForEndpoint,
  type ProviderPreset,
  type ProviderPresetProtocolRoute,
} from "./provider-presets.ts";

/**
 * Migration planning for configurable HTTP destinations created before their
 * official preset declared extra protocolRoutes. The destination's effective
 * routes win verbatim: missing preset routes are appended only, existing
 * routes and their custom URLs are never rewritten, and the route cap still
 * applies. Persisting stays on the regular destination PATCH path, which
 * keeps Key grants untouched (no credential is authorized for the appended
 * endpoints).
 */
export interface PresetProtocolMigrationPlan {
  /** Effective routes plus the appended preset routes, ready for a draft. */
  routes: DestinationProtocolRouteDraft[];
  /** Preset routes actually appended. */
  added: ProviderPresetProtocolRoute[];
  /** Missing preset routes left out because the route cap is already full. */
  dropped: ProviderPresetProtocolRoute[];
}

function presetRoutesForDestination(
  preset: ProviderPreset,
  destination: Destination,
): ProviderPresetProtocolRoute[] | undefined {
  if (preset.endpointUrl === "" && preset.id) {
    return providerPresetRoutesForEndpoint(
      { id: preset.id, endpointUrl: preset.endpointUrl, protocolRoutes: preset.protocolRoutes },
      destinationDraftRoutes(destination)[0]?.endpoint_url ?? "",
    );
  }
  return preset.protocolRoutes;
}

/** Null when the preset declares nothing the destination is missing. */
export function planPresetProtocolMigration(
  destination: Destination,
  preset: ProviderPreset,
): PresetProtocolMigrationPlan | null {
  const presetRoutes = presetRoutesForDestination(preset, destination);
  if (!presetRoutes || presetRoutes.length === 0) return null;
  const effective = destinationDraftRoutes(destination);
  const used = new Set(effective.map((route) => route.protocol));
  const missing = presetRoutes.filter((route) => !used.has(route.protocol));
  if (missing.length === 0) return null;
  const capacity = Math.max(0, MAX_HTTP_PROTOCOL_ROUTES - effective.length);
  const added = missing.slice(0, capacity);
  if (added.length === 0) return null;
  const dropped = missing.slice(capacity);
  return {
    routes: [
      ...effective,
      ...added.map((route) => ({
        protocol: route.protocol,
        endpoint_url: route.endpointUrl,
        auth_scheme: route.authScheme,
      })),
    ],
    added,
    dropped,
  };
}
