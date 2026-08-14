import { runBundledScript } from "../lib/run-bundled-script.js";

export async function runGit(args) {
  await runBundledScript("git.sh", args);
}
