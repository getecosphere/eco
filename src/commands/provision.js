import { runBundledScript } from "../lib/run-bundled-script.js";

export async function runProvision(args) {
  await runBundledScript("provision.sh", args);
}
