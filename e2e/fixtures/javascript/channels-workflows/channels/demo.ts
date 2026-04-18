import { defineChannel } from "tako.sh";

export default defineChannel("demo", {
  auth: async () => true,
});
