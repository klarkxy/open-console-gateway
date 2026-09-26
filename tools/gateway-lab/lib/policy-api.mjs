import { readJsonResponse, request } from "./dashboard.mjs";
import { POLICY_PATHS } from "./policy-contract.mjs";

export function classifyConfigProbe(status) {
  if (status === 200) return { ready: true, missing: false, code: "ready" };
  if (status === 404) return { ready: false, missing: true, code: "not_found" };
  if (status === 410) return { ready: false, missing: true, code: "tombstone" };
  if (status === 401) return { ready: false, missing: false, code: "unauthorized" };
  return { ready: false, missing: false, code: `http_${status}` };
}

export async function probePolicyApi(gatewayBase) {
  const response = await request(gatewayBase, POLICY_PATHS.config, "GET");
  const parsed = await readJsonResponse(response);
  const classification = classifyConfigProbe(parsed.status);
  return {
    ...classification,
    status: parsed.status,
    body: parsed.body,
    text: parsed.text,
    reason:
      classification.code === "ready"
        ? "configuration API responded 200"
        : classification.missing
          ? `GET ${POLICY_PATHS.config} returned ${parsed.status}; configuration API is not in this binary`
          : `GET ${POLICY_PATHS.config} returned ${parsed.status}: ${parsed.text.slice(0, 240)}`,
  };
}

export async function readContract(api) {
  return api.json("/dashboard/api/v4/contract");
}

export async function getPolicyConfig(gatewayBase) {
  const response = await request(gatewayBase, POLICY_PATHS.config, "GET");
  return readJsonResponse(response);
}

export async function putPolicyConfig(gatewayBase, { rules, expectedRevision, processGeneration }) {
  const response = await request(gatewayBase, POLICY_PATHS.config, "PUT", {
    rules,
    expectedRevision,
    processGeneration,
  });
  return readJsonResponse(response);
}

export async function putPolicyConfigCas(api, gatewayBase, rules, expectation) {
  const cas = expectation || (await readContract(api));
  return putPolicyConfig(gatewayBase, {
    rules,
    expectedRevision: cas.revision,
    processGeneration: cas.processGeneration,
  });
}

export async function getRestrictions(gatewayBase) {
  const response = await request(gatewayBase, POLICY_PATHS.restrictions, "GET");
  return readJsonResponse(response);
}

export async function clearRestriction(gatewayBase, id, expectation) {
  const response = await request(gatewayBase, POLICY_PATHS.clear(id), "POST", expectation || {});
  return readJsonResponse(response);
}

export async function clearRestrictionCas(api, gatewayBase, id) {
  const cas = await readContract(api);
  return clearRestriction(gatewayBase, id, {
    expectedRevision: cas.revision,
    processGeneration: cas.processGeneration,
  });
}

export function requireOk(parsed, label) {
  if (parsed.status < 200 || parsed.status >= 300) {
    throw new Error(`${label}: ${parsed.status} ${String(parsed.text || "").slice(0, 500)}`);
  }
  return parsed.body;
}

export function restrictionList(body) {
  if (!body) return [];
  if (Array.isArray(body.restrictions)) return body.restrictions;
  if (Array.isArray(body)) return body;
  return [];
}

export function destinationIdForSlot(destinations, slot, credentials = []) {
  const fromCred = (credentials || []).find(
    (row) => row.legacyAccountId === slot.accountId || row.id === slot.credentialId,
  );
  if (fromCred?.destinationId) return fromCred.destinationId;
  const name = `Gateway Lab ${slot.slot}`;
  const match =
    (destinations || []).find((row) => row.id && row.name === name) ||
    (destinations || []).find((row) => row.baseUrl && slot.url && row.baseUrl === new URL(slot.url).origin) ||
    (destinations || []).find((row) => row.name && String(row.name).includes(slot.slot));
  return match?.id || slot.connectionId || null;
}

export async function listDestinations(api) {
  const body = await api.json("/dashboard/api/v4/destinations");
  return body.destinations || [];
}

export async function listCredentials(api) {
  return api.credentials();
}

export function waitingRestrictions(body, predicate = () => true) {
  return restrictionList(body).filter((row) => row.state === "waiting" && predicate(row));
}
