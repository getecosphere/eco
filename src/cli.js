import { runAdopt } from "./commands/adopt.js";
import { runConfigure } from "./commands/configure.js";
import { runClearStarterProject } from "./commands/clearstarterproject.js";
import { runCompose } from "./commands/compose.js";
import { runDev } from "./commands/dev.js";
import { runCt } from "./commands/ct.js";
import { runDb } from "./commands/db.js";
import { runDashboard } from "./commands/dashboard.js";
import { runGit } from "./commands/git.js";
import { showHelp } from "./commands/help.js";
import { runInit } from "./commands/init.js";
import { runInstall } from "./commands/install.js";
import { runProxy } from "./commands/proxy.js";
import { runProx } from "./commands/prox.js";
import { runPorts } from "./commands/ports.js";
import { runProvision } from "./commands/provision.js";
import { runRepos } from "./commands/repos.js";
import { runShow } from "./commands/show.js";
import { runSync, runSyncStaging } from "./commands/sync.js";
import { runTree } from "./commands/tree.js";
import { runStartProject } from "./commands/startproject.js";
import { runUpdate } from "./commands/update.js";
import { runExpose, runUp } from "./commands/up.js";
import { runWebhookClean } from "./commands/webhook-clean.js";
import { runRust } from "./commands/rust.js";
import { runSendemail } from "./commands/sendemail.js";
import { runStress } from "./commands/stress.js";

export async function runCli(argv) {
  const [command = "help", ...rest] = argv;

  switch (command) {
    case "help":
    case "--help":
    case "-h":
      showHelp();
      return;
    case "init":
      await runInit(rest);
      return;
    case "install":
      await runInstall(rest);
      return;
    case "configure":
      await runConfigure(rest);
      return;
    case "show":
      await runShow(rest);
      return;
    case "tree":
      await runTree(rest);
      return;
    case "repos":
      await runRepos(rest);
      return;
    case "startproject":
      await runStartProject(rest);
      return;
    case "adopt":
      await runAdopt(rest);
      return;
    case "clearstarterproject":
      await runClearStarterProject(rest);
      return;
    case "compose":
      await runCompose(rest);
      return;
    case "provision":
      await runProvision(rest);
      return;
    case "proxy":
      await runProxy(rest);
      return;
    case "prox":
      await runProx(rest);
      return;
    case "ports":
      await runPorts(rest);
      return;
    case "update":
      await runUpdate(rest);
      return;
    case "up":
      await runUp(rest);
      return;
    case "expose":
      await runExpose(rest);
      return;
    case "webhook-clean":
      await runWebhookClean(rest);
      return;
    case "git":
      await runGit(rest);
      return;
    case "ct":
      await runCt(rest);
      return;
    case "db":
      await runDb(rest);
      return;
    case "dev":
      await runDev(rest);
      return;
    case "dashboard":
      await runDashboard(rest);
      return;
    case "rust":
      await runRust(rest);
      return;
    case "stress":
      await runStress(rest);
      return;
    case "sync":
      await runSync(rest);
      return;
    case "sync-staging":
      await runSyncStaging(rest);
      return;
    case "sendemail":
      await runSendemail(rest);
      return;
    default:
      throw new Error(`Unknown command: ${command}\n\nRun "eco help" for usage.`);
  }
}
