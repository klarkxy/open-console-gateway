import type {
  Destination,
  DestinationCredential,
  DestinationPatchInput,
} from "../api/destinations.ts";
import type { ConnectionEndpoint } from "../api/connections.ts";
import {
  DestinationEditError,
  buildDestinationPatch,
  destinationGrantCandidates,
  destinationRouteChanged,
  type DestinationEditDraft,
  type DestinationEditIssue,
  type DestinationGrantCandidate,
} from "./destination-edit.ts";

/**
 * One save attempt for the destination editor: validate the draft, then decide
 * whether the PATCH can go straight through or must first collect the explicit
 * Key-grant consent. The view renders purely from this plan's semantic status.
 */
export type DestinationSavePlan =
  | { status: "invalid"; issue: DestinationEditIssue }
  | { status: "patch"; input: DestinationPatchInput }
  | {
    status: "grant_consent";
    input: DestinationPatchInput;
    candidates: DestinationGrantCandidate[];
  };

export function planDestinationSave(
  destination: Destination,
  credentials: readonly DestinationCredential[],
  draft: DestinationEditDraft,
  endpoints: readonly ConnectionEndpoint[] = [],
): DestinationSavePlan {
  let input: DestinationPatchInput;
  try {
    input = buildDestinationPatch(destination, draft);
  } catch (error) {
    if (error instanceof DestinationEditError) return { status: "invalid", issue: error.issue };
    throw error;
  }
  if (destinationRouteChanged(destination, input)) {
    const candidates = destinationGrantCandidates(destination, credentials, input, endpoints);
    if (candidates.length > 0) {
      return { status: "grant_consent", input, candidates };
    }
  }
  return { status: "patch", input };
}
