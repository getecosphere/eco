import { runBundledScript } from "../lib/run-bundled-script.js";

export async function runInit(args) {
  await runBundledScript("init.sh", args);
}
