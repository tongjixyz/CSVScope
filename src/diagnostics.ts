import { invoke, isTauri } from "@tauri-apps/api/core";

const requests = new Map<number, { command: string; started: number }>();
let nextRequest = 0;

// Track command names and durations, never argument values or CSV cell contents.
export function diagnosticInvoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  const id = ++nextRequest;
  requests.set(id, { command, started: performance.now() });
  return invoke<T>(command, args).finally(() => requests.delete(id));
}

function pendingRequests() {
  return [...requests.values()].slice(0, 16).map(request => ({
    command: request.command, elapsedMs: Math.round(performance.now() - request.started),
  }));
}

function report(level: "error" | "warn", event: string, details: Record<string, unknown>) {
  if (!isTauri()) return;
  void invoke("frontend_log", { level, event, details }).catch(() => {});
}

export function reportFrontendError(event: string, error: unknown) {
  report("error", event, {
    message: String(error).slice(0, 3000),
    stack: error instanceof Error ? error.stack?.slice(0, 6000) : undefined,
    pendingRequests: pendingRequests(),
  });
}

export function installDiagnostics() {
  if (!isTauri()) return;
  window.addEventListener("error", event => {
    report("error", "frontend_error", {
      message: event.message.slice(0, 3000), file: event.filename,
      line: event.lineno, column: event.colno,
      stack: event.error instanceof Error ? event.error.stack?.slice(0, 6000) : undefined,
    });
  });
  window.addEventListener("unhandledrejection", event => reportFrontendError("frontend_unhandled_rejection", event.reason));
  let previous = performance.now();
  let wasVisible = document.visibilityState === "visible";
  let heartbeatPending = false;
  const heartbeat = () => {
    const now = performance.now();
    const visible = document.visibilityState === "visible";
    if (visible && wasVisible && now - previous >= 4000) {
      report("warn", "frontend_event_loop_delay", {
        gapMs: Math.round(now - previous), pendingRequests: pendingRequests(),
      });
    }
    previous = now;
    wasVisible = visible;
    if (heartbeatPending) return;
    heartbeatPending = true;
    void invoke("frontend_heartbeat", { visible, pending: pendingRequests() })
      .catch(() => {}).finally(() => { heartbeatPending = false; });
  };
  heartbeat();
  setInterval(heartbeat, 2000);
  document.addEventListener("visibilitychange", heartbeat);
}
