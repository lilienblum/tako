const PID_TEMPLATE_TOKEN = "{pid}";

export function resolveAppSocketPath(
  socketPath: string | undefined,
  pid: number = process.pid,
): string | undefined {
  if (!socketPath) {
    return undefined;
  }
  return socketPath.replaceAll(PID_TEMPLATE_TOKEN, String(pid));
}
