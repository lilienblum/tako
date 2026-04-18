import { Channel, defineWorkflow } from "tako.sh";

interface Payload {
  id: string;
  message: string;
}

export default defineWorkflow<Payload>(async (payload, ctx) => {
  await ctx.sleep("wait", 3_000);

  const ch = new Channel("demo-broadcast");
  await ch.publish({
    type: "message",
    data: { id: payload.id, message: payload.message },
  });
});
