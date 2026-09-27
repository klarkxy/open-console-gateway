export type GatewayLogLevel = "" | "TRACE" | "DEBUG" | "INFO" | "WARN" | "ERROR";

const levels = new Set<GatewayLogLevel>(["TRACE", "DEBUG", "INFO", "WARN", "ERROR"]);

export function parseGatewayLogLevel(value: string | null): GatewayLogLevel {
  const normalized = value?.toUpperCase() ?? "";
  return levels.has(normalized as GatewayLogLevel) ? normalized as GatewayLogLevel : "";
}

export function gatewayLogLevelTag(value: string): "default" | "primary" | "info" | "warning" | "error" {
  switch (parseGatewayLogLevel(value)) {
    case "DEBUG": return "primary";
    case "INFO": return "info";
    case "WARN": return "warning";
    case "ERROR": return "error";
    default: return "default";
  }
}
