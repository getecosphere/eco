import { runBundledScript } from "../lib/run-bundled-script.js";

export async function runConfigure(args) {
  await runBundledScript("configure.sh", args, { scope: "estate" });
}
