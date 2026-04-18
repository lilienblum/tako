import { afterEach, describe, expect, test } from "bun:test";
import { ChannelRegistry, setChannelSocketPublisher } from "../../src/channels";
import { defineChannel } from "../../src/channels/define";
import { buildChannelAccessor } from "../../src/channels/accessor";

afterEach(() => {
  setChannelSocketPublisher(null);
});

describe("buildChannelAccessor", () => {
  test("exposes unparameterized channel as direct object", () => {
    const reg = new ChannelRegistry();
    reg.register("status", defineChannel("status", { auth: async () => true }));
    const access = buildChannelAccessor(reg) as {
      status: { send: unknown };
    };
    expect(typeof access.status.send).toBe("function");
  });

  test("exposes parameterized channel as callable returning a handle", () => {
    const reg = new ChannelRegistry();
    reg.register(
      "chat",
      defineChannel<{ msg: { text: string } }>("chat/:roomId", {
        auth: async () => true,
        handler: { msg: async (d) => d },
      }),
    );
    const access = buildChannelAccessor(reg) as {
      chat: (p: { roomId: string }) => { send: unknown; subscribe: unknown; connect?: unknown };
    };
    const handle = access.chat({ roomId: "r1" });
    expect(typeof handle.send).toBe("function");
    expect(typeof handle.subscribe).toBe("function");
    expect(typeof handle.connect).toBe("function");
  });

  test("send routes through the socket publisher using the expanded channel name", async () => {
    const reg = new ChannelRegistry();
    reg.register(
      "chat",
      defineChannel<{ msg: { text: string } }>("chat/:roomId", {
        auth: async () => true,
        handler: { msg: async (d) => d },
      }),
    );
    let captured: { channel: string; message: unknown } | null = null;
    setChannelSocketPublisher(async (channel, message) => {
      captured = { channel, message };
      return {
        id: "1",
        channel,
        type: (message as { type: string }).type,
        data: (message as { data: unknown }).data,
      };
    });

    const access = buildChannelAccessor(reg) as {
      chat: (p: { roomId: string }) => {
        send: (type: string, data: unknown) => Promise<unknown>;
      };
    };
    await access.chat({ roomId: "r1" }).send("msg", { text: "hi" });

    expect(captured).toEqual({
      channel: "chat/r1",
      message: { type: "msg", data: { text: "hi" } },
    });
  });

  test("param expansion url-encodes values with reserved characters", () => {
    const reg = new ChannelRegistry();
    reg.register("chat", defineChannel("chat/:roomId", { auth: async () => true }));
    let captured: { channel: string } | null = null;
    setChannelSocketPublisher(async (channel, message) => {
      captured = { channel };
      return { id: "1", channel, type: "x", data: message };
    });
    const access = buildChannelAccessor(reg) as {
      chat: (p: { roomId: string }) => { send: (t: string, d: unknown) => Promise<unknown> };
    };
    return access
      .chat({ roomId: "room with spaces" })
      .send("x", {})
      .then(() => {
        expect(captured?.channel).toBe("chat/room%20with%20spaces");
      });
  });

  test("throws when a required param is missing", () => {
    const reg = new ChannelRegistry();
    reg.register("chat", defineChannel("chat/:roomId", { auth: async () => true }));
    const access = buildChannelAccessor(reg) as {
      chat: (p: Record<string, string>) => { send: unknown };
    };
    expect(() => access.chat({})).toThrow(/missing channel param 'roomId'/);
  });
});
