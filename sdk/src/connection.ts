/**
 * Tako Server Connection
 *
 * Manages the connection to tako-server for status reporting and control messages.
 */

import type {
  AppToServerMessage,
  ServerToAppMessage,
  ServerAck,
  TakoOptions,
} from "./types";

/**
 * Connection to tako-server management socket
 */
export class ServerConnection {
  private socket: ReturnType<typeof Bun.connect> | null = null;
  private heartbeatInterval: ReturnType<typeof setInterval> | null = null;
  private connected = false;
  private shuttingDown = false;
  private pendingMessages: string[] = [];
  private responsePromise: {
    resolve: (value: ServerAck) => void;
    reject: (error: Error) => void;
  } | null = null;

  constructor(
    private readonly socketPath: string,
    private readonly appName: string,
    private readonly version: string,
    private readonly instanceId: number,
    private readonly appSocketPath: string,
    private readonly options: TakoOptions = {}
  ) {}

  /**
   * Connect to tako-server and send ready signal
   */
  async connect(): Promise<ServerAck> {
    return new Promise((resolve, reject) => {
      const self = this;

      this.socket = Bun.connect({
        unix: this.socketPath,
        socket: {
          open(socket) {
            self.connected = true;
            console.log(`[tako] Connected to server at ${self.socketPath}`);

            // Send any pending messages
            for (const msg of self.pendingMessages) {
              socket.write(msg + "\n");
            }
            self.pendingMessages = [];
          },

          data(socket, data) {
            const text = data.toString().trim();
            if (!text) return;

            try {
              const message = JSON.parse(text) as ServerAck | ServerToAppMessage;

              // Check if this is an ack response
              if ("status" in message && (message.status === "ack" || message.status === "error")) {
                if (self.responsePromise) {
                  self.responsePromise.resolve(message as ServerAck);
                  self.responsePromise = null;
                }
                return;
              }

              // Handle server-to-app messages
              self.handleServerMessage(message as ServerToAppMessage);
            } catch (err) {
              console.error("Failed to parse server message:", text, err);
            }
          },

          close() {
            self.connected = false;
            console.log("Disconnected from server");
          },

          error(socket, error) {
            console.error("Connection error:", error);
            if (self.responsePromise) {
              self.responsePromise.reject(error);
              self.responsePromise = null;
            }
          },

          connectError(socket, error) {
            console.error("Failed to connect:", error);
            reject(error);
          },
        },
      });

      // Set up response promise for ready ack
      this.responsePromise = { resolve, reject };

      // Send ready message
      this.sendReady();
    });
  }

  /**
   * Send ready signal to tako-server
   */
  private sendReady(): void {
    const message: AppToServerMessage = {
      type: "ready",
      app: this.appName,
      version: this.version,
      instance_id: this.instanceId,
      pid: process.pid,
      socket_path: this.appSocketPath,
      timestamp: new Date().toISOString(),
    };
    this.send(message);
  }

  /**
   * Start the heartbeat loop
   */
  startHeartbeat(): void {
    if (this.heartbeatInterval) return;

    this.heartbeatInterval = setInterval(() => {
      if (!this.shuttingDown) {
        this.sendHeartbeat();
      }
    }, 1000);
  }

  /**
   * Send heartbeat to tako-server
   */
  private sendHeartbeat(): void {
    const message: AppToServerMessage = {
      type: "heartbeat",
      app: this.appName,
      instance_id: this.instanceId,
      pid: process.pid,
      timestamp: new Date().toISOString(),
    };
    this.send(message);
  }

  /**
   * Handle messages from tako-server
   */
  private handleServerMessage(message: ServerToAppMessage): void {
    switch (message.type) {
      case "shutdown":
        console.log(`[tako] Received shutdown signal: ${message.reason}`);
        this.handleShutdown(message.drain_timeout_seconds);
        break;

      case "reload_config":
        console.log("Received config reload signal");
        this.handleConfigReload(message.secrets);
        break;

      default:
        console.warn("Unknown message type:", (message as any).type);
    }
  }

  /**
   * Handle graceful shutdown
   */
  private async handleShutdown(drainTimeoutSeconds: number): Promise<void> {
    this.shuttingDown = true;

    // Stop heartbeat
    if (this.heartbeatInterval) {
      clearInterval(this.heartbeatInterval);
      this.heartbeatInterval = null;
    }

    // Give time for in-flight requests to complete
    // In a real implementation, we'd track active requests
    await new Promise((resolve) =>
      setTimeout(resolve, Math.min(drainTimeoutSeconds * 1000, 5000))
    );

    // Send shutdown ack
    const ackMessage: AppToServerMessage = {
      type: "shutdown_ack",
      app: this.appName,
      instance_id: this.instanceId,
      pid: process.pid,
      drained: true,
      timestamp: new Date().toISOString(),
    };
    this.send(ackMessage);

    // Close connection and exit
    this.close();
    process.exit(0);
  }

  /**
   * Handle config reload
   */
  private async handleConfigReload(
    secrets: Record<string, string>
  ): Promise<void> {
    // Update environment variables
    for (const [key, value] of Object.entries(secrets)) {
      process.env[key] = value;
    }

    // Call user's reload handler if provided
    if (this.options.onConfigReload) {
      try {
        await this.options.onConfigReload(secrets);
      } catch (err) {
        console.error("Error in onConfigReload handler:", err);
      }
    }
  }

  /**
   * Send a message to tako-server
   */
  private send(message: AppToServerMessage): void {
    const json = JSON.stringify(message);

    if (this.connected && this.socket) {
      (this.socket as any).write(json + "\n");
    } else {
      // Queue message until connected
      this.pendingMessages.push(json);
    }
  }

  /**
   * Close the connection
   */
  close(): void {
    if (this.heartbeatInterval) {
      clearInterval(this.heartbeatInterval);
      this.heartbeatInterval = null;
    }

    if (this.socket) {
      (this.socket as any).end();
      this.socket = null;
    }

    this.connected = false;
  }

  /**
   * Check if connected
   */
  isConnected(): boolean {
    return this.connected;
  }

  /**
   * Check if shutting down
   */
  isShuttingDown(): boolean {
    return this.shuttingDown;
  }
}
