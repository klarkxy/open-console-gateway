import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

export const DEFAULT_DEV_GATEWAY_PORT = "19042";

export function devEnvironment(source = process.env) {
  return {
    ...source,
    OCG_GATEWAY_PORT: source.OCG_GATEWAY_PORT?.trim() || DEFAULT_DEV_GATEWAY_PORT,
    OCG_DEBUG_REQUESTS: source.OCG_DEBUG_REQUESTS?.trim() || "1",
    OCG_DEBUG_DIR: source.OCG_DEBUG_DIR?.trim() || fileURLToPath(new URL("../.artifacts/debug-requests", import.meta.url)),
    OCG_LOG_LEVEL: source.OCG_LOG_LEVEL?.trim() || "debug",
  };
}

const isMain = process.argv[1]
  && fileURLToPath(import.meta.url).toLowerCase() === process.argv[1].toLowerCase();

if (isMain) {
  const tauriCli = fileURLToPath(new URL("../node_modules/@tauri-apps/cli/tauri.js", import.meta.url));
  const env = devEnvironment();
  console.log(`Gateway development port: ${env.OCG_GATEWAY_PORT}`);
  console.log(`Runtime log level: ${env.OCG_LOG_LEVEL}`);
  console.log(`Request capture: ${env.OCG_DEBUG_REQUESTS === "1" ? env.OCG_DEBUG_DIR : "disabled"}`);

  const child = spawn(process.execPath, [tauriCli, "dev"], {
    cwd: process.cwd(),
    env,
    stdio: "inherit",
    windowsHide: false,
  });

  for (const signal of ["SIGINT", "SIGTERM"]) {
    process.once(signal, () => {
      if (!child.killed) child.kill(signal);
    });
  }

  child.once("error", (error) => {
    console.error(`Failed to start Tauri development mode: ${error.message}`);
    process.exitCode = 1;
  });
  child.once("exit", (code, signal) => {
    process.exitCode = code ?? (signal === "SIGINT" ? 130 : 1);
  });
}
