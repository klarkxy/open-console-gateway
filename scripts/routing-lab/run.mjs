export { makeApi } from "../../tools/gateway-lab/lib/dashboard.mjs";
import { isDirectRun } from "../../tools/gateway-lab/lib/common.mjs";
import { runRoutingLab } from "../../tools/gateway-lab/lib/routing-compat.mjs";

export { runRoutingLab };

if (isDirectRun(import.meta.url)) {
  await runRoutingLab(process.argv.slice(2));
}
