#!/usr/bin/env node
import { isDirectRun } from "./lib/common.mjs";
import { POLICY_HELP, resolvePolicyVerifyInput, runPolicyVerify } from "./lib/policy-verify-run.mjs";

export { POLICY_HELP, resolvePolicyVerifyInput, runPolicyVerify };

export async function main(argv = process.argv.slice(2)) {
  const input = resolvePolicyVerifyInput(argv);
  if (input.help) {
    process.stdout.write(POLICY_HELP);
    return 0;
  }
  const report = await runPolicyVerify(input);
  return Number.isInteger(process.exitCode) ? process.exitCode : Number(report?.exitCode) || 0;
}

if (isDirectRun(import.meta.url)) {
  try {
    const code = await main();
    process.exitCode = Number.isInteger(code) ? code : process.exitCode;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    process.stderr.write(`${message}\n`);
    if (error?.code === "usage") process.stderr.write(POLICY_HELP);
    process.exitCode = 1;
  }
}
