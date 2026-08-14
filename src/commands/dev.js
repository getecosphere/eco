import { execFile } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

async function runFlushDns() {
  if (process.platform !== "darwin") {
    process.stderr.write("eco dev flushdns: only supported on macOS\n");
    process.exit(1);
  }

  process.stdout.write("Flushing DNS cache...\n");

  // macOS DNS flush: clear the system DNS cache then kick mDNSResponder.
  // Both steps are needed -- dscacheutil clears the userland cache while
  // the kill -HUP restarts the system resolver daemon that holds its own
  // in-memory cache. Requires sudo (will prompt if not already root).
  await execFileAsync("sudo", ["dscacheutil", "-flushcache"]);
  await execFileAsync("sudo", ["killall", "-HUP", "mDNSResponder"]);

  process.stdout.write("DNS cache flushed.\n");
}

export async function runDev(args) {
  const [subcommand, ...rest] = args;

  switch (subcommand) {
    case "flushdns":
      await runFlushDns(rest);
      return;
    case undefined:
    case "help":
    case "--help":
    case "-h":
      process.stdout.write(
        `eco dev - local development utilities

Usage:
  eco dev flushdns    Flush the macOS DNS cache (runs dscacheutil + mDNSResponder restart)
`
      );
      return;
    default:
      throw new Error(`Unknown dev subcommand: ${subcommand}\n\nRun "eco dev help" for usage.`);
  }
}
