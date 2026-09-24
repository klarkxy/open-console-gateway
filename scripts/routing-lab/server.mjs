export { createLab, selfCheck, SLOT_DEFS } from "../../tools/gateway-lab/lib/lab.mjs";
import { createLab, selfCheck } from "../../tools/gateway-lab/lib/lab.mjs";
import { isDirectRun } from "../../tools/gateway-lab/lib/common.mjs";

if (process.argv.includes("--self-check")) {
  selfCheck();
  process.stdout.write("self-check ok\n");
  process.exit(0);
}

if (
  (isDirectRun(import.meta.url) || process.argv[1]?.endsWith("server.mjs")) &&
  !process.argv.includes("--self-check") &&
  process.argv.includes("--listen")
) {
  const lab = createLab();
  const started = await lab.start();
  process.stdout.write(`${JSON.stringify({ listeners: started.listeners }, null, 2)}\n`);
}
