import crypto from "node:crypto";
import { readFile } from "node:fs/promises";
import http from "node:http";
import { spawn } from "node:child_process";

function timingSafeEqualHex(left, right) {
  const leftBuffer = Buffer.from(left, "utf8");
  const rightBuffer = Buffer.from(right, "utf8");
  if (leftBuffer.length !== rightBuffer.length) {
    return false;
  }
  return crypto.timingSafeEqual(leftBuffer, rightBuffer);
}

async function readConfig() {
  const configPath = process.env.ECO_WEBHOOK_CONFIG;
  if (!configPath) {
    throw new Error("ECO_WEBHOOK_CONFIG is required.");
  }
  const text = await readFile(configPath, "utf8");
  return JSON.parse(text);
}

async function main() {
  const config = await readConfig();
  const pathName = config.path || "/__eco/github/deploy";
  const port = Number(process.env.ECO_WEBHOOK_PORT || config.port || 8790);
  const debounceMs = Number(process.env.ECO_WEBHOOK_DEBOUNCE_MS || config.debounceMs || 15000);
  let activeChild = null;
  let deployRunning = false;
  let pendingRepos = new Set();
  let activeRepos = new Set();
  let debounceTimer = null;

  function log(message, details = "") {
    const suffix = details ? ` ${details}` : "";
    process.stdout.write(`[eco webhook] ${message}${suffix}\n`);
  }

  function formatRepos(repos) {
    return [...repos].sort().join(",");
  }

  function scheduleDeploy(triggerRepo, triggerBranch) {
    if (debounceTimer) {
      clearTimeout(debounceTimer);
      debounceTimer = null;
    }

    debounceTimer = setTimeout(() => {
      debounceTimer = null;
      if (deployRunning) {
        log("deploy-wait", `active repos=${formatRepos(pendingRepos)}`);
        scheduleDeploy(triggerRepo, triggerBranch);
        return;
      }
      startDeploy(triggerRepo, triggerBranch);
    }, debounceMs);

    log("deploy-scheduled", `trigger=${triggerRepo} branch=${triggerBranch || config.branch} delay_ms=${debounceMs} pending=${formatRepos(pendingRepos)}`);
  }

  function startDeploy(triggerRepo, triggerBranch) {
    if (deployRunning) {
      return;
    }

    deployRunning = true;
    activeRepos = new Set(pendingRepos);
    pendingRepos = new Set();

    const branch = triggerBranch || config.branch;
    const repoList = formatRepos(activeRepos);
    log("deploy-start", `${triggerRepo} ${branch} repos=${repoList || triggerRepo}`);
    activeChild = spawn("/bin/bash", [config.deployCommand, triggerRepo], {
      cwd: config.projectRoot,
      stdio: "inherit",
      env: {
        ...process.env,
        ECO_DEPLOY_PENDING_REPOS: repoList,
        ECO_DEPLOY_TRIGGER_REPO: triggerRepo,
        ECO_DEPLOY_TRIGGER_BRANCH: branch,
        ECO_DEPLOY_MODE: config.branchPolicy === "any-except-main" ? "staging" : "prod"
      }
    });

    activeChild.on("exit", (code, signal) => {
      log(
        "deploy-finished",
        `repo=${triggerRepo} code=${code ?? ""} signal=${signal ?? ""} repos=${repoList || triggerRepo}`.trim()
      );
      activeChild = null;
      deployRunning = false;
      activeRepos = new Set();

      if (pendingRepos.size > 0) {
        const nextRepo = [...pendingRepos][0] || triggerRepo;
        log("deploy-reschedule", `next=${nextRepo} repos=${formatRepos(pendingRepos)}`);
        scheduleDeploy(nextRepo, triggerBranch);
      }
    });

    activeChild.on("error", (error) => {
      log("deploy-error", error?.message || "unknown");
      activeChild = null;
      deployRunning = false;
      activeRepos = new Set();

      if (pendingRepos.size > 0) {
        const nextRepo = [...pendingRepos][0] || triggerRepo;
        log("deploy-reschedule", `next=${nextRepo} repos=${formatRepos(pendingRepos)}`);
        scheduleDeploy(nextRepo, triggerBranch);
      }
    });
  }

  const server = http.createServer((req, res) => {
    if (req.method === "GET" && req.url === "/health") {
      log("health", req.socket.remoteAddress || "");
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ ok: true, service: `${config.project}-deploy-webhook` }));
      return;
    }

    if (req.method !== "POST" || req.url !== pathName) {
      log("not-found", `${req.method || "?"} ${req.url || ""}`.trim());
      res.writeHead(404, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ error: "Not found" }));
      return;
    }

    const chunks = [];
    req.on("data", (chunk) => {
      chunks.push(chunk);
    });
    req.on("end", () => {
      const body = Buffer.concat(chunks);
      const event = String(req.headers["x-github-event"] || "");
      log("received", `${event || "unknown-event"} bytes=${body.length}`);
      const signature = String(req.headers["x-hub-signature-256"] || "");
      const expected = `sha256=${crypto.createHmac("sha256", config.secret).update(body).digest("hex")}`;
      if (!signature || !timingSafeEqualHex(signature, expected)) {
        log("signature-invalid", event || "unknown-event");
        res.writeHead(401, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ error: "Invalid signature" }));
        return;
      }

      if (event === "ping") {
        log("ping-ok");
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ ok: true, event }));
        return;
      }

      if (event !== "push") {
        log("ignored-event", event);
        res.writeHead(202, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ ok: true, ignored: true, event }));
        return;
      }

      let payload;
      try {
        payload = JSON.parse(body.toString("utf8"));
      } catch {
        res.writeHead(400, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ error: "Invalid JSON payload" }));
        return;
      }

      const fullName = payload?.repository?.full_name || "";
      const ref = payload?.ref || "";
      const allowedRepo = Array.isArray(config.repos)
        ? config.repos.find((repo) => repo.fullName === fullName)
        : null;
      if (!allowedRepo) {
        log("ignored-repo", fullName || "<missing>");
        res.writeHead(202, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ ok: true, ignored: true, reason: "repo-not-configured" }));
        return;
      }

      const branchPolicy = config.branchPolicy || "fixed";
      let acceptedBranch = "";
      if (branchPolicy === "any-except-main") {
        // Staging: accept pushes to any branch except the prod deploy branch.
        // The prod branch keeps meaning "deploy to production", so it must
        // never also land on staging.
        const pushed = ref.startsWith("refs/heads/") ? ref.slice("refs/heads/".length) : "";
        if (!pushed) {
          log("ignored-ref", `${fullName} ${ref}`);
          res.writeHead(202, { "Content-Type": "application/json" });
          res.end(JSON.stringify({ ok: true, ignored: true, reason: "not-a-branch-ref", ref }));
          return;
        }
        if (pushed === config.branch) {
          log("ignored-branch", `${fullName} ${ref}`);
          res.writeHead(202, { "Content-Type": "application/json" });
          res.end(JSON.stringify({ ok: true, ignored: true, reason: "branch-is-prod-deploy-branch", ref }));
          return;
        }
        acceptedBranch = pushed;
      } else if (ref !== `refs/heads/${config.branch}`) {
        log("ignored-branch", `${fullName} ${ref}`);
        res.writeHead(202, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ ok: true, ignored: true, reason: "branch-mismatch", ref }));
        return;
      } else {
        acceptedBranch = config.branch;
      }

      pendingRepos.add(fullName);
      scheduleDeploy(fullName, acceptedBranch);

      res.writeHead(202, { "Content-Type": "application/json" });
      res.end(JSON.stringify({
        ok: true,
        accepted: true,
        branch: acceptedBranch,
        branchPolicy,
        deployRunning,
        debounceMs,
        pending: [...pendingRepos],
        repo: fullName
      }));
    });
  });

  server.listen(port, "0.0.0.0", () => {
    process.stdout.write(`[eco webhook] listening on :${port}${pathName}\n`);
  });
}

main().catch((error) => {
  process.stderr.write(`${error.stack || error.message}\n`);
  process.exit(1);
});
