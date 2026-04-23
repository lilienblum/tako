import { defineChannel } from "tako.sh";
import type { MissionChannelUpdate } from "../src/server/types";

export default defineChannel("mission-log/:base").$messageTypes<{
  update: MissionChannelUpdate;
}>();
