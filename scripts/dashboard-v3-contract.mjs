import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { createDashboardContract } from "./lib/dashboard-contract.mjs";

export const {
  SCHEMA_RELATIVE_PATH,
  TYPES_RELATIVE_PATH,
  ARTIFACT_PATHS,
  CARGO_EXAMPLE,
  CARGO_ARGS,
  parseArgs,
  normalizeNewlines,
  assertTypesAreContractOnly,
  assertArtifactsMatch,
  renderTypeScript,
  exportSchemaFromCargo,
  runContract,
} = createDashboardContract("v3");

const invokedDirectly = Boolean(process.argv[1])
  && import.meta.url === pathToFileURL(resolve(process.argv[1])).href;
if (invokedDirectly) {
  const mode = parseArgs(process.argv.slice(2));
  await runContract(mode);
}
