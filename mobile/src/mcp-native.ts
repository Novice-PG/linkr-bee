import { Capacitor, registerPlugin } from "@capacitor/core";

/**
 * Native half of the Linkr Bee MCP server.
 *
 * This module only exposes the Android transport to the app: the HTTP server,
 * bearer token and lifecycle live in Java, while every MCP protocol request and
 * tool call is answered by web/mcp_server.js through the `httpRequest` event.
 */

export type McpBind = "loopback" | "lan";

export type McpState = {
  running: boolean;
  port: number;
  bind: McpBind;
  /** 64 lowercase hex chars, generated natively and never logged. */
  token: string;
  url: string;
  urls: string[];
  foreground: boolean;
  activeRequests: number;
  lastError: string | null;
};

export type McpHttpRequest = {
  id: string;
  method: "POST";
  path: string;
  body: string;
  remote: string;
  userAgent: string;
};

export type McpStartConfig = {
  port: number;
  bind: McpBind;
};

export type McpRespondInput = {
  id: string;
  status: number;
  body: string;
};

export interface McpListenerHandle {
  remove(): Promise<void>;
}

export interface LinkrMcpBridge {
  available: true;
  platform: "android";
  start(config: McpStartConfig): Promise<McpState>;
  stop(): Promise<McpState>;
  getState(): Promise<McpState>;
  regenerateToken(): Promise<McpState>;
  respond(input: McpRespondInput): Promise<void>;
  addListener(
    event: "httpRequest",
    listener: (payload: McpHttpRequest) => void,
  ): Promise<McpListenerHandle>;
  addListener(
    event: "stateChanged",
    listener: (state: McpState) => void,
  ): Promise<McpListenerHandle>;
}

type NativeLinkrMcp = {
  start(options: McpStartConfig): Promise<McpState>;
  stop(): Promise<McpState>;
  getState(): Promise<McpState>;
  regenerateToken(): Promise<McpState>;
  respond(options: McpRespondInput): Promise<void>;
  // Capacitor resolves the overloads on the JS side; the wrapper below keeps
  // the public bridge typed per event.
  addListener(
    eventName: string,
    listener: (...args: any[]) => any,
  ): Promise<McpListenerHandle>;
};

const NativeLinkrMcp = registerPlugin<NativeLinkrMcp>("LinkrMcp");

export function createLinkrMcpBridge(): LinkrMcpBridge {
  const bridge = {
    available: true as const,
    platform: "android" as const,
    start: (config: McpStartConfig) => NativeLinkrMcp.start(config),
    stop: () => NativeLinkrMcp.stop(),
    getState: () => NativeLinkrMcp.getState(),
    regenerateToken: () => NativeLinkrMcp.regenerateToken(),
    respond: (input: McpRespondInput) => NativeLinkrMcp.respond(input),
    addListener: (event: string, listener: (data: any) => void) =>
      NativeLinkrMcp.addListener(event, listener),
  };
  return bridge as unknown as LinkrMcpBridge;
}

/**
 * Installs window.LinkrMcp on Android only. The bridge is synchronous (like
 * window.LinkrNativeBle) because app modules read it while their body runs.
 */
export function installLinkrMcpBridge(): void {
  if (!Capacitor.isNativePlatform() || Capacitor.getPlatform() !== "android") {
    return;
  }
  if (!window.LinkrMcp) {
    window.LinkrMcp = createLinkrMcpBridge();
  }
}

declare global {
  interface Window {
    LinkrMcp?: LinkrMcpBridge;
  }
}
