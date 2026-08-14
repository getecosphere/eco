import https from "node:https";
import { loadProjectDeployment, resolveDeployGithubConfig, pctExecCapture } from "./up.js";
import { getCloudflareEnv } from "./proxy.js";

const GITHUB_API = "https://api.github.com";
const CF_API = "https://api.cloudflare.com/client/v4";

function log(message) {
  process.stdout.write(`[eco webhook-clean] ${message}\n`);
}

async function githubRequest(pathname, token, { method = "GET", body } = {}) {
  const response = await fetch(`${GITHUB_API}${pathname}`, {
    method,
    headers: {
      Authorization: `Bearer ${token}`,
      Accept: "application/vnd.github+json",
      "X-GitHub-Api-Version": "2022-11-28",
      "User-Agent": "eco-webhook-clean"
    },
    body: body ? JSON.stringify(body) : undefined
  });
  const text = await response.text();
  const payload = text ? JSON.parse(text) : null;
  if (!response.ok) {
    throw new Error(`GitHub API ${response.status} ${pathname}: ${payload?.message || text}`);
  }
  return payload;
}

async function cloudflareApi(pathname, token, { method = "GET", body } = {}) {
  const response = await fetch(`${CF_API}${pathname}`, {
    method,
    headers: { Authorization: `Bearer ${token}` },
    body: body ? JSON.stringify(body) : undefined
  });
  const payload = await response.json();
  if (!response.ok || payload.success === false) {
    const message = (payload?.errors || []).map((error) => error.message).join("; ") || response.statusText;
    throw new Error(`Cloudflare API ${response.status} ${pathname}: ${message}`);
  }
  return payload.result;
}

// Resolve through Cloudflare's public DoH so the check mirrors public DNS,
// not the local resolver. A hostname with no A/AAAA answer is treated as
// broken and safe to prune.
function hostnameResolves(hostname) {
  return new Promise((resolvePromise) => {
    const url = `https://cloudflare-dns.com/dns-query?name=${encodeURIComponent(hostname)}&type=A`;
    const request = https.get(url, { headers: { accept: "application/dns-json" } }, (response) => {
      let data = "";
      response.on("data", (chunk) => { data += chunk; });
      response.on("end", () => {
        try {
          const parsed = JSON.parse(data);
          const answers = Array.isArray(parsed?.Answer) ? parsed.Answer : [];
          resolvePromise(answers.some((answer) => answer?.type === 1));
        } catch {
          resolvePromise(false);
        }
      });
    });
    request.on("error", () => resolvePromise(false));
    request.setTimeout(8000, () => { request.destroy(); resolvePromise(false); });
  });
}

function splitRepo(fullName) {
  const parts = String(fullName || "").split("/");
  return { owner: parts[0] || "", repo: parts[1] || "" };
}

export async function runWebhookClean(args) {
  const dryRun = args.includes("--dry-run");
  const input = args.find((arg) => !arg.startsWith("--")) || ".";
  const githubToken = process.env.GITHUB_TOKEN || "";
  if (!githubToken) {
    throw new Error("GITHUB_TOKEN is required for webhook cleanup.");
  }

  const deployment = await loadProjectDeployment(input);
  const { project, expose, deploy, ctid, ctProjectRoot } = deployment;
  const githubDeploy = resolveDeployGithubConfig({ project, expose, deploy });
  if (!githubDeploy) {
    log(`${project} has no deploy.github.enabled configuration; nothing to clean.`);
    return;
  }
  const account = expose?.cloudflare_account || "";
  const currentUrl = githubDeploy.webhookUrl;
  const webhookPath = githubDeploy.path || "/__eco/github/deploy";

  log(`project=${project} currentWebhook=${currentUrl}${dryRun ? " (dry-run)" : ""}`);
  log(`account=${account || "(none)"}`);

  // Authoritative repo list from the estate's receiver config.
  let repos = [];
  try {
    const raw = await pctExecCapture(ctid, `cat ${ctProjectRoot}/.eco/deploy/github-webhook.json`);
    const config = JSON.parse(raw);
    repos = (Array.isArray(config?.repos) ? config.repos : []).map((entry) => entry?.fullName).filter(Boolean);
  } catch (error) {
    log(`could not read receiver config in CT ${ctid}: ${error?.message || error}`);
  }
  if (repos.length === 0) {
    log("no configured repos found; nothing to clean.");
    return;
  }

  // 1) GitHub webhooks: prune broken deploy hooks on this estate's repos.
  const brokenHosts = new Set();
  let removedHooks = 0;
  for (const fullName of repos) {
    const { owner, repo } = splitRepo(fullName);
    if (!owner || !repo) {
      continue;
    }
    let hooks;
    try {
      hooks = await githubRequest(`/repos/${owner}/${repo}/hooks`, githubToken);
    } catch (error) {
      log(`  skip ${fullName}: ${error?.message}`);
      continue;
    }
    for (const hook of Array.isArray(hooks) ? hooks : []) {
      const url = hook?.config?.url || "";
      if (!url.includes(webhookPath)) {
        continue; // not a deploy webhook
      }
      if (url === currentUrl) {
        continue; // never touch the live webhook
      }
      let hostname = "";
      try {
        hostname = new URL(url).hostname;
      } catch {
        continue;
      }
      const resolves = await hostnameResolves(hostname);
      if (resolves) {
        log(`  keep ${fullName}: ${url} (resolves)`);
        continue;
      }
      log(`  ${dryRun ? "would remove" : "removed"} ${fullName}: ${url} (hook ${hook.id}, unresolvable)`);
      brokenHosts.add(hostname);
      if (!dryRun) {
        try {
          await githubRequest(`/repos/${owner}/${repo}/hooks/${hook.id}`, githubToken, { method: "DELETE" });
          removedHooks += 1;
        } catch (error) {
          log(`  failed to remove ${fullName}: ${url} -> ${error?.message}`);
        }
      } else {
        removedHooks += 1;
      }
    }
  }
  log(`${dryRun ? "would remove" : "removed"} ${removedHooks} broken webhook(s)`);

  // 2) Cloudflare DNS: drop malformed relative records left behind by a
  //    sibling-apex hook hostname (e.g. hooks-stuff8.com.stuff8.com).
  if (account && brokenHosts.size > 0) {
    const env = getCloudflareEnv(account);
    if (!env.token || !env.zoneId) {
      log(`skipping DNS cleanup: Cloudflare env missing for account "${account}"`);
    } else {
      let zoneName = "";
      try {
        const zone = await cloudflareApi(`/zones/${env.zoneId}`, env.token);
        zoneName = zone?.name || "";
      } catch (error) {
        log(`skipping DNS cleanup: ${error?.message}`);
      }
      if (zoneName) {
        for (const hostname of brokenHosts) {
          // Only sibling-apex hostnames (not under the zone) leave a
          // malformed "<hostname>.<zone>" relative record behind.
          if (hostname.endsWith(`.${zoneName}`)) {
            continue;
          }
          for (const candidate of [hostname, `${hostname}.${zoneName}`]) {
            try {
              const records = await cloudflareApi(
                `/zones/${env.zoneId}/dns_records?name=${encodeURIComponent(candidate)}&per_page=100`,
                env.token
              );
              const match = Array.isArray(records)
                ? records.find((record) => record?.name === candidate)
                : null;
              if (match?.id) {
                log(`  ${dryRun ? "would remove" : "removed"} DNS record ${candidate} from zone ${zoneName}`);
                if (!dryRun) {
                  await cloudflareApi(`/zones/${env.zoneId}/dns_records/${match.id}`, env.token, { method: "DELETE" });
                }
              }
            } catch (error) {
              log(`  failed DNS cleanup ${candidate}: ${error?.message}`);
            }
          }
        }
      }
    }
  } else if (brokenHosts.size === 0) {
    log("no broken hook hostnames to reconcile in DNS");
  }
  log("done");
}
