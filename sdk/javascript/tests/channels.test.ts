import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { Tako } from "../src/tako";

describe("channels", () => {
  beforeEach(() => {
    Tako.channels.clear();
  });

  afterEach(() => {
    Tako.channels.clear();
    mock.restore();
  });

  test("creates channel handles from the global Tako API", () => {
    const channel = Tako.channels.create("chat:room-123");

    expect(channel.name).toBe("chat:room-123");
  });

  test("registers an exact auth definition via channels.create", async () => {
    const channel = Tako.channels.create("chat:room-123", {
      auth(_request, ctx) {
        expect(ctx.channel).toBe("chat:room-123");
        expect(ctx.operation).toBe("subscribe");
        return true;
      },
    });

    const result = await Tako.channels.authorize({
      channel: channel.name,
      operation: "subscribe",
      request: { url: "https://app.example.com/chat/room-123" },
    });

    expect(result.ok).toBe(true);
  });

  test("matches the most specific channel definition", async () => {
    Tako.channels.define("chat:*", {
      auth() {
        return false;
      },
    });
    Tako.channels.define("chat:room-123", {
      auth() {
        return { subject: "user-123" };
      },
    });

    const result = await Tako.channels.authorize({
      channel: "chat:room-123",
      operation: "subscribe",
      request: { url: "https://app.example.com/chat/room-123" },
    });

    expect(result).toEqual({
      ok: true,
      replayWindowMs: 86_400_000,
      inactivityTtlMs: 0,
      keepaliveIntervalMs: 25_000,
      maxConnectionLifetimeMs: 7_200_000,
      subject: "user-123",
    });
  });

  test("publish still works as an internal implementation detail", async () => {
    const fetchMock = mock(() =>
      Promise.resolve(
        new Response(JSON.stringify({ id: "42", channel: "chat:room-123" }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        }),
      ),
    );
    const originalFetch = globalThis.fetch;
    globalThis.fetch = fetchMock as typeof fetch;

    try {
      const channel = Tako.channels.create("chat:room-123");
      const response = await channel.publish(
        { type: "message", data: { text: "hi" } },
        { baseUrl: "https://app.example.com" },
      );

      expect(response.id).toBe("42");
      expect(fetchMock).toHaveBeenCalledTimes(1);

      const [url, init] = fetchMock.mock.calls[0]!;
      expect(url).toBe("https://app.example.com/channels/chat%3Aroom-123/messages");
      expect(init?.method).toBe("POST");
      expect(init?.headers).toEqual({ "Content-Type": "application/json" });
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  test("subscribe opens the canonical SSE route", () => {
    const eventSourceFactory = mock((url: string) => ({ url, kind: "eventsource", close() {} }));
    const webSocketFactory = mock((url: string) => ({ url, kind: "websocket" }));
    const channel = Tako.channels.create("chat:room-123");

    const subscription = channel.subscribe({
      baseUrl: "https://app.example.com",
      eventSourceFactory,
      webSocketFactory,
    });

    expect(subscription.transport).toBe("sse");
    expect(subscription.raw).toEqual({
      kind: "eventsource",
      url: "https://app.example.com/channels/chat%3Aroom-123",
      close: expect.any(Function),
    });
    expect(eventSourceFactory).toHaveBeenCalledTimes(1);
    expect(webSocketFactory).toHaveBeenCalledTimes(0);
  });

  test("connect targets the canonical websocket route with last_message_id", () => {
    const send = mock((_data: unknown) => {});
    const close = mock((_code?: number, _reason?: string) => {});
    const webSocketFactory = mock((url: string) => ({ url, kind: "websocket", send, close }));
    const channel = Tako.channels.create("chat:room-123", { transport: "ws" });

    const connection = channel.connect({
      baseUrl: "https://app.example.com",
      lastMessageId: "42",
      webSocketFactory,
    });

    expect(connection.transport).toBe("ws");
    expect(connection.raw).toEqual({
      kind: "websocket",
      url: "wss://app.example.com/channels/chat%3Aroom-123?last_message_id=42",
      send,
      close,
    });

    connection.send({ type: "typing" });
    connection.close(1000, "done");

    expect(send).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledWith(JSON.stringify({ type: "typing" }));
    expect(close).toHaveBeenCalledTimes(1);
  });

  test("subscribe remains read-only even when ws transport is enabled", () => {
    const eventSourceFactory = mock((url: string) => ({ url, kind: "eventsource", close() {} }));
    const channel = Tako.channels.create("chat:room-123", { transport: "ws" });

    const subscription = channel.subscribe({
      baseUrl: "https://app.example.com",
      eventSourceFactory,
    });

    expect(subscription.transport).toBe("sse");
    expect("send" in subscription).toBe(false);
  });

  test("returns lifecycle config from channel authorization", async () => {
    Tako.channels.define("chat:*", {
      auth() {
        return { subject: "user-123" };
      },
      replayWindowMs: 86_400_000,
      inactivityTtlMs: 0,
      keepaliveIntervalMs: 25_000,
      maxConnectionLifetimeMs: 7_200_000,
      transport: "ws",
    });

    const result = await Tako.channels.authorize({
      channel: "chat:room-123",
      operation: "subscribe",
      request: { url: "https://app.example.com/chat/room-123" },
    });

    expect(result).toEqual({
      ok: true,
      subject: "user-123",
      replayWindowMs: 86_400_000,
      inactivityTtlMs: 0,
      keepaliveIntervalMs: 25_000,
      maxConnectionLifetimeMs: 7_200_000,
      transport: "ws",
    });
  });
});
