#!/usr/bin/env node
import { isDirectRun, parseArgv } from "./lib/common.mjs";
import { selfCheck } from "./lib/lab.mjs";
import { HELP, registerCommand, resetCommand, scenarioCommand, serveCommand, verifyCommand } from "./lib/commands.mjs";

function resolvedExitCode() {
  return Number.isInteger(process.exitCode) ? process.exitCode : 0;
}

export async function main(argv = process.argv.slice(2)) {
  if (argv.includes("--self-check")) {
    selfCheck();
    process.stdout.write("self-check ok\n");
    return 0;
  }
  const { command, flags, positional } = parseArgv(argv);
  if (!command || command === "help" || flags.help || flags.h) {
    process.stdout.write(HELP);
    return 0;
  }
  if (command === "serve") await serveCommand(flags);
  else if (command === "register") await registerCommand(flags);
  else if (command === "scenario") await scenarioCommand(flags);
  else if (command === "reset") await resetCommand(flags);
  else if (command === "verify") return verifyCommand(flags, positional);
  else {
    process.stderr.write(`unknown command: ${command}\n${HELP}`);
    return 2;
  }
  return resolvedExitCode();
}

export async function runCli(argv = process.argv.slice(2)) {
  try {
    const code = await main(argv);
    if (Number.isInteger(code)) process.exitCode = code;
    return Number.isInteger(process.exitCode) ? process.exitCode : 0;
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
    return 1;
  }
}

if (isDirectRun(import.meta.url)) {
  await runCli();
}
