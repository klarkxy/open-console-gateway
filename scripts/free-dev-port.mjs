import { execFileSync } from "node:child_process";
import { cwd } from "node:process";
import net from "node:net";

const port = Number(process.env.OCG_VITE_PORT) || 30001;

if (process.platform !== "win32") {
  process.exit(0);
}

await new Promise((resolve, reject) => {
  const server = net
    .createServer()
    .once("error", (error) => {
      if (error.code === "EADDRINUSE") {
        resolve();
        return;
      }
      reject(error);
    })
    .once("listening", () => {
      server.close(() => process.exit(0));
    })
    .listen(port, "127.0.0.1");
});

const workspace = cwd().toLowerCase().replaceAll("\\", "/");
const script = `
$ErrorActionPreference = "SilentlyContinue"
$rows = Get-NetTCPConnection -LocalPort ${port} -State Listen |
  ForEach-Object {
    $process = Get-CimInstance Win32_Process -Filter "ProcessId=$($_.OwningProcess)"
    if ($process) {
      [pscustomobject]@{ Id = $process.ProcessId; CommandLine = $process.CommandLine }
    }
  }
$rows | ConvertTo-Json -Compress
`;

const output = execFileSync("powershell.exe", ["-NoProfile", "-Command", script], {
  encoding: "utf8",
}).trim();

if (!output) {
  process.exit(0);
}

const rows = [].concat(JSON.parse(output));
const foreign = [];

for (const row of rows) {
  const commandLine = String(row.CommandLine || "");
  const normalized = commandLine.toLowerCase().replaceAll("\\", "/");
  const isThisVite = normalized.includes(workspace) && normalized.includes("vite");

  if (!isThisVite) {
    foreign.push(`${row.Id}: ${commandLine}`);
    continue;
  }

  execFileSync("powershell.exe", ["-NoProfile", "-Command", `Stop-Process -Id ${row.Id} -Force`]);
  console.log(`Stopped stale Vite dev server on port ${port} (pid ${row.Id}).`);
}

if (foreign.length > 0) {
  console.error(`Port ${port} is in use by a non-project process:`);
  for (const line of foreign) {
    console.error(`  ${line}`);
  }
  process.exit(1);
}
