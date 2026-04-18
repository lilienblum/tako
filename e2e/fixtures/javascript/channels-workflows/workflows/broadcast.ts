import { Channel, defineWorkflow } from "tako.sh";

interface Payload {
  message: string;
}

export default defineWorkflow<Payload>(async (payload, ctx) => {
  await ctx.sleep("wait", 500);

  const ch = new Channel("demo");
  await ch.publish({ type: "message", data: { message: payload.message } });
});
