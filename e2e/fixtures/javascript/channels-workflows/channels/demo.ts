import { defineChannel } from "tako.sh";

interface Messages {
  message: { message: string };
}

export default defineChannel("demo", {
  auth: async () => true,
}).$messageTypes<Messages>();
