import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

const DEFAULT_CLOUDFLARED_CONFIG = "/etc/cloudflared/config.yml";

function proxyHelp() {
  process.stdout.write(`eco proxy

Manage ingress/proxy infrastructure helpers.

Usage:
  eco proxy migrate-cloudflared [proxy|ctid] [--dry-run] [--stop-host]
  eco proxy init-tunnel [proxy|ctid] <hostname> [--name <tunnel-name>] [--account <name>] [--dry-run]
  eco proxy tunnel-replicas <account> [count] [--target <ctid>] [--dry-run]
  eco prox tunnel-replicas <account> [count] [--target <ctid>] [--dry-run]

Options:
  --dry-run    Print the migration plan without executing it
  --stop-host  Stop and disable host-level cloudflared after CT service starts
  --name       Override the tunnel name for init-tunnel
  --account    Named Cloudflare account to use (reads CF_API_TOKEN_<NAME>,
               CF_ACCOUNT_ID_<NAME>, CF_ZONE_ID_<NAME> instead of the
               unsuffixed defaults) -- lets one host manage tunnels/DNS
               across multiple Cloudflare accounts. Matches ecompose.yml's
               expose.cloudflare_account.
  --target     Target proxy CT id or hostname (tunnel-replicas; default "proxy")

Examples:
  eco proxy migrate-cloudflared
  eco proxy migrate-cloudflared 100
  eco proxy migrate-cloudflared proxy --dry-run
  eco proxy migrate-cloudflared proxy --stop-host
  eco proxy init-tunnel proxy assessment.ktt.my.id
  eco proxy init-tunnel proxy training.jogjaitcamp.com --account jogjaitcamp
  eco proxy tunnel-replicas jogjaitcamp 3
  eco proxy tunnel-replicas jogjaitcamp 3 --target proxy --dry-run
  eco proxy tunnel-replicas jogjaitcamp
`);
}

function parseOptions(args) {
  const options = {};
  const positionals = [];

  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    if (!arg.startsWith("--")) {
      positionals.push(arg);
      continue;
    }

    const key = arg.slice(2);
    if (key === "dry-run" || key === "stop-host") {
      options[key] = true;
      continue;
    }

    if (key === "name" || key === "account" || key === "target") {
      const value = args[i + 1];
      if (!value || value.startsWith("--")) {
        throw new Error(`Missing value for option --${key}`);
      }
      options[key] = value;
      i += 1;
      continue;
    }

    throw new Error(`Unknown option --${key}`);
  }

  return { options, positionals };
}

function runCommand(command, args, cwd) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      stdio: "inherit",
      env: process.env
    });

    child.on("error", reject);
    child.on("exit", (code, signal) => {
      if (signal) {
        reject(new Error(`${command} terminated by signal ${signal}`));
        return;
      }
      if (code !== 0) {
        reject(new Error(`${command} exited with code ${code}`));
        return;
      }
      resolve();
    });
  });
}

function runCapture(command, args, cwd) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      stdio: ["ignore", "pipe", "pipe"],
      env: process.env
    });

    let stdout = "";
    let stderr = "";

    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });

    child.on("error", reject);
    child.on("exit", (code, signal) => {
      if (signal) {
        reject(new Error(`${command} terminated by signal ${signal}`));
        return;
      }
      resolve({ code: code ?? 1, stdout, stderr });
    });
  });
}

async function pathExists(filePath) {
  try {
    await stat(filePath);
    return true;
  } catch {
    return false;
  }
}

async function ensureHostCloudflaredFiles() {
  const configPath = "/etc/cloudflared/config.yml";
  if (!(await pathExists(configPath))) {
    throw new Error(`Missing host cloudflared config: ${configPath}`);
  }

  const configText = await readFile(configPath, "utf8");
  const credentialsMatch = configText.match(/^credentials-file:\s*(.+)\s*$/m);
  if (!credentialsMatch) {
    return {
      configPath,
      credentialsMode: "token",
      credentialFiles: []
    };
  }

  const credentialsPath = credentialsMatch[1].trim().replace(/^["']|["']$/g, "");
  if (!(await pathExists(credentialsPath))) {
    throw new Error(`Missing host cloudflared credentials file: ${credentialsPath}`);
  }

  return {
    configPath,
    credentialsMode: "file",
    credentialFiles: [credentialsPath]
  };
}

async function resolveHostCloudflaredServiceFile() {
  const directPath = "/etc/systemd/system/cloudflared.service";
  if (await pathExists(directPath)) {
    return directPath;
  }

  const systemctlResult = await runCapture("systemctl", ["cat", "cloudflared"]);
  if (systemctlResult.code !== 0 || !systemctlResult.stdout.trim()) {
    throw new Error("Cannot resolve host cloudflared.service definition.");
  }

  const tempDir = await mkdtemp(path.join(tmpdir(), "eco-cloudflared-host-unit-"));
  const filePath = path.join(tempDir, "cloudflared.service");
  await writeFile(filePath, systemctlResult.stdout, "utf8");
  return filePath;
}

async function resolveCtIdByHostname(hostname) {
  const result = await runCapture("pct", ["list"]);
  if (result.code !== 0) {
    throw new Error(`pct list failed with code ${result.code}: ${result.stderr}`.trim());
  }

  for (const rawLine of result.stdout.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line || /^VMID\s+/i.test(line)) {
      continue;
    }
    const ctid = line.split(/\s+/)[0];
    if (!/^\d+$/.test(ctid)) {
      continue;
    }

    const config = await runCapture("pct", ["config", ctid]);
    if (config.code !== 0) {
      continue;
    }

    const hostnameMatch = config.stdout.match(/^hostname:\s*(.+)\s*$/m);
    if (hostnameMatch && hostnameMatch[1].trim() === hostname) {
      return ctid;
    }
  }

  throw new Error(`Cannot resolve CT by hostname "${hostname}" from pct list.`);
}

async function resolveCtInput(input) {
  if (!input) {
    return resolveCtIdByHostname("proxy");
  }
  if (/^\d+$/.test(String(input))) {
    return String(input);
  }
  return resolveCtIdByHostname(String(input));
}

async function ensureCtRunning(ctid) {
  const status = await runCapture("pct", ["status", String(ctid)]);
  if (status.code === 0 && /status:\s+running/.test(status.stdout)) {
    return;
  }
  await runCommand("pct", ["start", String(ctid)]);
}

async function pctExec(ctid, command) {
  await runCommand("pct", ["exec", String(ctid), "--", "bash", "-lc", command]);
}

async function pctExecCapture(ctid, command) {
  const result = await runCapture("pct", ["exec", String(ctid), "--", "bash", "-lc", command]);
  if (result.code !== 0) {
    throw new Error(`pct exec ${ctid} failed with code ${result.code}: ${result.stderr || result.stdout}`.trim());
  }
  return result.stdout;
}

async function pushFileToCt(ctid, sourcePath, targetPath) {
  await runCommand("pct", ["push", String(ctid), sourcePath, targetPath]);
}

function escapeSingleQuotes(value) {
  return String(value).replace(/'/g, `'\\''`);
}

function slugifyTunnelName(hostname) {
  return String(hostname)
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 48) || "eco-proxy";
}

async function resolveTunnelIdByName(ctid, tunnelName) {
  const output = await pctExecCapture(
    ctid,
    `cloudflared tunnel list 2>/dev/null | awk 'NR>1 && $2 == "${tunnelName}" { print $1; exit }'`
  );
  const tunnelId = output.trim();
  if (!tunnelId) {
    throw new Error(`Cannot resolve tunnel ID for "${tunnelName}" inside CT ${ctid}.`);
  }
  return tunnelId;
}

function normalizeCloudflareAccountKey(account) {
  return String(account)
    .trim()
    .toUpperCase()
    .replace(/[^A-Z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");
}

// Backward-compatible default: no `account` (or a falsy one) reads the
// original unsuffixed CF_API_TOKEN/CF_ACCOUNT_ID/CF_ZONE_ID, so existing
// single-Cloudflare-account hosts need no env changes. A named account
// (from ecompose.yml's `expose.cloudflare_account`) reads
// CF_*_<NORMALIZED_NAME> instead, letting one Proxmox host hold credentials
// for several Cloudflare accounts side by side (e.g. one per customer
// domain) and have each estate pick the right one explicitly. There is
// deliberately no fallback from a named account to the unsuffixed default
// -- silently reusing the wrong account's token/zone would write DNS/tunnel
// changes into the wrong Cloudflare account instead of failing loudly.
export function getCloudflareEnv(account) {
  if (!account) {
    return {
      token: process.env.CF_API_TOKEN || "",
      accountId: process.env.CF_ACCOUNT_ID || "",
      zoneId: process.env.CF_ZONE_ID || ""
    };
  }

  const key = normalizeCloudflareAccountKey(account);
  return {
    token: process.env[`CF_API_TOKEN_${key}`] || "",
    accountId: process.env[`CF_ACCOUNT_ID_${key}`] || "",
    zoneId: process.env[`CF_ZONE_ID_${key}`] || ""
  };
}

export function hasCloudflareApiEnv(account) {
  const env = getCloudflareEnv(account);
  return Boolean(env.token && env.accountId && env.zoneId);
}

async function cloudflareApi(pathname, init, account) {
  const { token } = getCloudflareEnv(account);
  if (!token) {
    const envVar = account ? `CF_API_TOKEN_${normalizeCloudflareAccountKey(account)}` : "CF_API_TOKEN";
    throw new Error(`${envVar} is required for Cloudflare API automation${account ? ` (account "${account}")` : ""}.`);
  }

  const response = await fetch(`https://api.cloudflare.com/client/v4${pathname}`, {
    ...init,
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
      ...(init?.headers || {})
    }
  });

  const payload = await response.json();
  if (!response.ok || payload.success === false) {
    const message = payload?.errors?.map((error) => error.message).join("; ") || response.statusText;
    throw new Error(`Cloudflare API request failed: ${message}`);
  }

  return payload.result;
}

function requireEnvVar(value, baseName, account) {
  if (value) {
    return;
  }
  const envVar = account ? `${baseName}_${normalizeCloudflareAccountKey(account)}` : baseName;
  throw new Error(`${envVar} is required for Cloudflare automation${account ? ` (account "${account}")` : ""}.`);
}

export async function overwriteDnsRecordForTunnel(hostname, tunnelId, account) {
  const { zoneId } = getCloudflareEnv(account);
  requireEnvVar(zoneId, "CF_ZONE_ID", account);

  const target = `${tunnelId}.cfargotunnel.com`;
  const existing = await cloudflareApi(
    `/zones/${zoneId}/dns_records?name=${encodeURIComponent(hostname)}&per_page=100`,
    { method: "GET" },
    account
  );

  const matching = Array.isArray(existing)
    ? existing.find((record) => record.name === hostname && ["A", "AAAA", "CNAME"].includes(record.type))
    : null;

  const body = JSON.stringify({
    type: "CNAME",
    name: hostname,
    content: target,
    proxied: true,
    ttl: 1
  });

  if (matching) {
    await cloudflareApi(`/zones/${zoneId}/dns_records/${matching.id}`, {
      method: "PUT",
      body
    }, account);
  } else {
    await cloudflareApi(`/zones/${zoneId}/dns_records`, {
      method: "POST",
      body
    }, account);
  }

  // A successful write response alone is not sufficient after a hostname was
  // delegated away and later returned to Cloudflare. Read the authoritative
  // zone record back, so `eco up` never claims exposure was repaired while it
  // still points at an old tunnel or an unrelated target.
  const verified = await cloudflareApi(
    `/zones/${zoneId}/dns_records?name=${encodeURIComponent(hostname)}&per_page=100`,
    { method: "GET" },
    account
  );
  const record = Array.isArray(verified)
    ? verified.find((entry) => entry.name === hostname && entry.type === "CNAME")
    : null;
  if (!record || String(record.content || "").replace(/\.$/, "") !== target) {
    throw new Error(`Cloudflare DNS verification failed for ${hostname}; expected CNAME ${target}.`);
  }
  return matching ? "updated" : "created";
}

export async function removeDnsRecordForTunnel(hostname, tunnelId, account) {
  const { zoneId } = getCloudflareEnv(account);
  requireEnvVar(zoneId, "CF_ZONE_ID", account);
  const target = `${tunnelId}.cfargotunnel.com`;
  const existing = await cloudflareApi(
    `/zones/${zoneId}/dns_records?name=${encodeURIComponent(hostname)}&per_page=100`,
    { method: "GET" },
    account
  );
  const matching = Array.isArray(existing)
    ? existing.find((record) => record.name === hostname && record.type === "CNAME" && String(record.content || "").replace(/\.$/, "") === target)
    : null;
  if (!matching) return false;
  await cloudflareApi(`/zones/${zoneId}/dns_records/${matching.id}`, { method: "DELETE" }, account);
  return true;
}

export async function removeRemoteTunnel(tunnelId, account) {
  const { accountId } = getCloudflareEnv(account);
  requireEnvVar(accountId, "CF_ACCOUNT_ID", account);
  await cloudflareApi(
    `/accounts/${accountId}/cfd_tunnel/${encodeURIComponent(tunnelId)}`,
    { method: "DELETE" },
    account
  );
}

async function listRemoteTunnelsByName(tunnelName, account) {
  const { accountId } = getCloudflareEnv(account);
  requireEnvVar(accountId, "CF_ACCOUNT_ID", account);

  const result = await cloudflareApi(
    `/accounts/${accountId}/cfd_tunnel?is_deleted=false&per_page=1000`,
    { method: "GET" },
    account
  );
  const tunnels = Array.isArray(result) ? result : [];
  return tunnels.filter((tunnel) => tunnel?.name === tunnelName);
}

async function getTunnelTokenById(tunnelId, account) {
  const { accountId } = getCloudflareEnv(account);
  requireEnvVar(accountId, "CF_ACCOUNT_ID", account);

  const result = await cloudflareApi(
    `/accounts/${accountId}/cfd_tunnel/${encodeURIComponent(tunnelId)}/token`,
    { method: "GET" },
    account
  );
  if (!result) {
    throw new Error(`Cloudflare did not return a token for tunnel ${tunnelId}.`);
  }
  return String(result).trim();
}

async function ensureRemoteTunnel(tunnelName, account) {
  const { accountId } = getCloudflareEnv(account);
  requireEnvVar(accountId, "CF_ACCOUNT_ID", account);

  const existing = await listRemoteTunnelsByName(tunnelName, account);
  const active = existing.find((tunnel) => !tunnel.deleted_at);
  if (active) {
    return {
      tunnelId: active.id,
      tunnelName: active.name,
      tunnelToken: active.token || await getTunnelTokenById(active.id, account),
      created: false
    };
  }

  const created = await cloudflareApi(`/accounts/${accountId}/cfd_tunnel`, {
    method: "POST",
    body: JSON.stringify({
      name: tunnelName,
      config_src: "cloudflare"
    })
  }, account);

  return {
    tunnelId: created.id,
    tunnelName: created.name || tunnelName,
    tunnelToken: created.token || await getTunnelTokenById(created.id, account),
    created: true
  };
}

async function getRemoteTunnelConfig(tunnelId, account) {
  const { accountId } = getCloudflareEnv(account);
  requireEnvVar(accountId, "CF_ACCOUNT_ID", account);

  const result = await cloudflareApi(
    `/accounts/${accountId}/cfd_tunnel/${encodeURIComponent(tunnelId)}/configurations`,
    { method: "GET" },
    account
  );

  return result?.config || {};
}

export async function putRemoteTunnelConfig(tunnelId, hostname, serviceUrl, account) {
  const { accountId } = getCloudflareEnv(account);
  requireEnvVar(accountId, "CF_ACCOUNT_ID", account);

  const existingConfig = await getRemoteTunnelConfig(tunnelId, account).catch(() => ({}));
  const ingress = Array.isArray(existingConfig?.ingress) ? existingConfig.ingress : [];
  const nonFallback = ingress.filter((rule) => rule?.hostname || rule?.service !== "http_status:404");
  const fallback = ingress.find((rule) => !rule?.hostname && rule?.service === "http_status:404") || {
    service: "http_status:404"
  };

  let replaced = false;
  const nextIngress = nonFallback.map((rule) => {
    if (rule?.hostname === hostname) {
      replaced = true;
      return {
        ...rule,
        hostname,
        service: serviceUrl,
        originRequest: rule?.originRequest || {}
      };
    }
    return rule;
  });

  if (!replaced) {
    nextIngress.push({
      hostname,
      service: serviceUrl,
      originRequest: {}
    });
  }

  await cloudflareApi(
    `/accounts/${accountId}/cfd_tunnel/${encodeURIComponent(tunnelId)}/configurations`,
    {
      method: "PUT",
      body: JSON.stringify({
        config: {
          ...existingConfig,
          ingress: [...nextIngress, fallback]
        }
      })
    },
    account
  );

  // Token-managed tunnels ignore local ingress rules. Confirm the remote
  // configuration that cloudflared will actually consume contains this exact
  // hostname and origin before reporting a completed expose.
  const verifiedConfig = await getRemoteTunnelConfig(tunnelId, account);
  const verifiedRule = Array.isArray(verifiedConfig?.ingress)
    ? verifiedConfig.ingress.find((rule) => rule?.hostname === hostname)
    : null;
  if (!verifiedRule || verifiedRule.service !== serviceUrl) {
    throw new Error(`Cloudflare tunnel route verification failed for ${hostname}; expected origin ${serviceUrl}.`);
  }
}

export async function removeRemoteTunnelHostname(tunnelId, hostname, account) {
  const { accountId } = getCloudflareEnv(account);
  requireEnvVar(accountId, "CF_ACCOUNT_ID", account);
  const existingConfig = await getRemoteTunnelConfig(tunnelId, account);
  const ingress = Array.isArray(existingConfig?.ingress) ? existingConfig.ingress : [];
  const nextIngress = ingress.filter((rule) => rule?.hostname !== hostname);
  if (nextIngress.length === ingress.length) return false;
  await cloudflareApi(
    `/accounts/${accountId}/cfd_tunnel/${encodeURIComponent(tunnelId)}/configurations`,
    {
      method: "PUT",
      body: JSON.stringify({
        config: {
          ...existingConfig,
          ingress: nextIngress
        }
      })
    },
    account
  );
  return true;
}

// A Cloudflare Tunnel belongs to exactly one Cloudflare account, so a
// second account (e.g. a customer domain managed under a separate
// Cloudflare login) needs its own cloudflared process, not just its own
// API credentials -- one running `cloudflared tunnel run` only ever serves
// the single tunnel named in its own config's `tunnel:` line. The default,
// unsuffixed account keeps the original fixed path/unit name so existing
// proxy CTs need no changes; a named account gets its own sibling config
// dir and systemd unit so multiple tunnels can run side by side in the
// same proxy CT.
export function cloudflaredConfigPathForAccount(account) {
  if (!account) {
    return DEFAULT_CLOUDFLARED_CONFIG;
  }
  return `/etc/cloudflared-${slugifyTunnelName(account)}/config.yml`;
}

export function cloudflaredServiceNameForAccount(account) {
  if (!account) {
    return "cloudflared";
  }
  return `cloudflared-${slugifyTunnelName(account)}`;
}

async function writeTempServiceFile(tunnelToken = "", configPath = DEFAULT_CLOUDFLARED_CONFIG) {
  const execStart = tunnelToken
    ? `ExecStart=/usr/bin/cloudflared --no-autoupdate --config ${configPath} tunnel run --token ${tunnelToken}`
    : `ExecStart=/usr/bin/cloudflared --no-autoupdate --config ${configPath} tunnel run`;
  const serviceText = `[Unit]
Description=cloudflared
After=network-online.target
Wants=network-online.target

[Service]
TimeoutStartSec=15
Type=notify
${execStart}
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=multi-user.target
`;

  const tempDir = await mkdtemp(path.join(tmpdir(), "eco-cloudflared-"));
  const filePath = path.join(tempDir, "cloudflared.service");
  await writeFile(filePath, serviceText, "utf8");
  return { tempDir, filePath };
}

async function installCtCloudflaredService(ctid, tunnelToken = "", serviceName = "cloudflared", configPath = DEFAULT_CLOUDFLARED_CONFIG) {
  const { tempDir, filePath } = await writeTempServiceFile(tunnelToken, configPath);
  try {
    await pushFileToCt(ctid, filePath, `/etc/systemd/system/${serviceName}.service`);
  } finally {
    await rm(tempDir, { recursive: true, force: true });
  }
}

async function verifyCloudflaredService(ctid, serviceName = "cloudflared") {
  await pctExec(
    ctid,
    [
      `if systemctl is-active --quiet ${serviceName}; then`,
      "  exit 0;",
      "fi;",
      `systemctl --no-pager --full status ${serviceName} || true;`,
      `journalctl -u ${serviceName} -n 80 --no-pager;`,
      "exit 1"
    ].join("\n")
  );
}

// Refresh the unit as well as Cloudflare's DNS and ingress configuration.
// Otherwise a unit left behind by an older setup can keep running a token for
// another tunnel, producing Cloudflare's fallback 404 despite a valid DNS
// record and a valid remote route.
export async function refreshRemoteTunnelService({
  ctid,
  tunnelId,
  serviceName,
  configPath,
  account
}) {
  const tunnelToken = await getTunnelTokenById(tunnelId, account);
  await installCtCloudflaredService(ctid, tunnelToken, serviceName, configPath);
  await pctExec(
    ctid,
    `systemctl daemon-reload && systemctl enable ${serviceName} && systemctl restart ${serviceName}`
  );
  await verifyCloudflaredService(ctid, serviceName);
}

async function ensureCloudflaredInstalled(ctid) {
  await pctExec(
    ctid,
    [
      "if ! command -v cloudflared >/dev/null 2>&1; then",
      "  apt-get update;",
      "  apt-get install -y curl ca-certificates;",
      "  arch=$(dpkg --print-architecture);",
      "  case \"$arch\" in",
      "    amd64) pkg=cloudflared-linux-amd64.deb ;;",
      "    arm64) pkg=cloudflared-linux-arm64.deb ;;",
      "    *) echo \"Unsupported architecture for cloudflared: $arch\" >&2; exit 1 ;;",
      "  esac;",
      "  curl -fsSL \"https://github.com/cloudflare/cloudflared/releases/latest/download/${pkg}\" -o /tmp/cloudflared.deb;",
      "  apt-get install -y /tmp/cloudflared.deb;",
      "  rm -f /tmp/cloudflared.deb;",
      "fi"
    ].join("\n")
  );
}

async function writeTunnelConfig(ctid, {
  tunnelId,
  tunnelToken,
  tunnelName,
  hostname,
  serviceUrl,
  configPath = DEFAULT_CLOUDFLARED_CONFIG
}) {
  const configText = [
    `tunnel: ${tunnelToken}`,
    `# eco-tunnel-id: ${tunnelId}`,
    `# eco-tunnel-name: ${tunnelName}`,
    "",
    "originRequest:",
    "  noTLSVerify: true",
    "",
    "ingress:",
    `  - hostname: ${hostname}`,
    `    service: ${serviceUrl}`,
    "  - service: http_status:404",
    ""
  ].join("\n");

  const shell = [
    `mkdir -p ${path.posix.dirname(configPath)}`,
    `cat >${configPath} <<'EOF'`,
    configText.replace(/\n$/, ""),
    "EOF"
  ].join("\n");
  await pctExec(ctid, shell);
}

export async function ensureProxyTunnel({
  target = "proxy",
  hostname,
  tunnelName,
  serviceUrl = "http://127.0.0.1:80",
  dryRun = false,
  nonInteractive = false,
  cloudflareAccount
}) {
  if (!hostname) {
    throw new Error(`Missing hostname.\n\nRun "eco proxy help" for usage.`);
  }

  const ctid = await resolveCtInput(target);
  const resolvedTunnelName = tunnelName || slugifyTunnelName(hostname);
  const configPath = cloudflaredConfigPathForAccount(cloudflareAccount);
  const serviceName = cloudflaredServiceNameForAccount(cloudflareAccount);
  const configDir = path.posix.dirname(configPath);

  const plan = [
    `Ensure CT ${ctid} is running`,
    `Ensure cloudflared is installed inside CT ${ctid}`,
    `Backup existing ${configPath} inside CT ${ctid} if present`,
    `Ensure ${configDir} and /root/.cloudflared exist in CT ${ctid}`,
    hasCloudflareApiEnv(cloudflareAccount)
      ? `Create or reuse remote tunnel "${resolvedTunnelName}" through Cloudflare API${cloudflareAccount ? ` (account "${cloudflareAccount}")` : ""}`
      : `Run interactive cloudflared tunnel login inside CT ${ctid} if cert.pem is missing`,
    hasCloudflareApiEnv(cloudflareAccount)
      ? `Create or update DNS record ${hostname} -> <tunnel-id>.cfargotunnel.com through Cloudflare API`
      : `Create DNS route ${hostname} -> tunnel ${resolvedTunnelName}`,
    `Write ${configPath} in CT ${ctid} with placeholder ingress ${serviceUrl}`,
    `Install ${serviceName}.service in CT ${ctid}`,
    `Enable and restart ${serviceName} in CT ${ctid}`,
    `Verify ${serviceName} status in CT ${ctid}`
  ];

  if (dryRun) {
    return { ctid, tunnelName: resolvedTunnelName, plan };
  }

  process.stdout.write(`[eco proxy] Initializing dedicated tunnel in CT ${ctid} for ${hostname}${cloudflareAccount ? ` (account "${cloudflareAccount}")` : ""}\n`);
  await ensureCtRunning(ctid);
  await ensureCloudflaredInstalled(ctid);
  await pctExec(
    ctid,
    `if [ -f ${configPath} ]; then cp ${configPath} ${configPath}.bak.$(date +%s); fi`
  );
  await pctExec(ctid, `mkdir -p ${configDir} /root/.cloudflared`);

  if (hasCloudflareApiEnv(cloudflareAccount)) {
    const remote = await ensureRemoteTunnel(resolvedTunnelName, cloudflareAccount);
    if (remote.created) {
      process.stdout.write(`[eco proxy] Created remote tunnel ${remote.tunnelName} (${remote.tunnelId})\n`);
    } else {
      process.stdout.write(`[eco proxy] Reusing remote tunnel ${remote.tunnelName} (${remote.tunnelId})\n`);
    }

    await overwriteDnsRecordForTunnel(hostname, remote.tunnelId, cloudflareAccount);
    await putRemoteTunnelConfig(remote.tunnelId, hostname, serviceUrl, cloudflareAccount);
    await writeTunnelConfig(ctid, {
      tunnelId: remote.tunnelId,
      tunnelToken: remote.tunnelToken,
      tunnelName: remote.tunnelName,
      hostname,
      serviceUrl,
      configPath
    });
    await installCtCloudflaredService(ctid, remote.tunnelToken, serviceName, configPath);
    await pctExec(ctid, `systemctl daemon-reload && systemctl enable ${serviceName} && systemctl restart ${serviceName}`);
    await verifyCloudflaredService(ctid, serviceName);
    return {
      ctid,
      tunnelName: remote.tunnelName,
      tunnelId: remote.tunnelId,
      configPath,
      serviceName
    };
  }

  if (nonInteractive) {
    const varSuffix = cloudflareAccount ? `_${normalizeCloudflareAccountKey(cloudflareAccount)}` : "";
    throw new Error(
      `Proxy tunnel is missing and non-interactive bootstrap was requested, but CF_API_TOKEN${varSuffix} / CF_ACCOUNT_ID${varSuffix} / CF_ZONE_ID${varSuffix} are not set${cloudflareAccount ? ` (account "${cloudflareAccount}")` : ""}.`
    );
  }

  const hasCert = await runCapture("pct", [
    "exec",
    String(ctid),
    "--",
    "bash",
    "-lc",
    "test -f /root/.cloudflared/cert.pem"
  ]);
  if (hasCert.code !== 0) {
    process.stdout.write(`[eco proxy] Browser login required for Cloudflare tunnel authorization\n`);
    await pctExec(ctid, "cloudflared tunnel login");
  }

  const tunnelExists = await runCapture("pct", [
    "exec",
    String(ctid),
    "--",
    "bash",
    "-lc",
    `cloudflared tunnel list 2>/dev/null | awk 'NR>1 && $2 == "${resolvedTunnelName}" { found=1 } END { exit found ? 0 : 1 }'`
  ]);
  if (tunnelExists.code !== 0) {
    await pctExec(ctid, `cloudflared tunnel create ${resolvedTunnelName}`);
  }

  const tunnelId = await resolveTunnelIdByName(ctid, resolvedTunnelName);
  try {
    await pctExec(ctid, `cloudflared tunnel route dns ${resolvedTunnelName} ${hostname}`);
  } catch (error) {
    if (/record with that host already exists/i.test(String(error.message))) {
      process.stdout.write(`[eco proxy] Existing DNS record detected for ${hostname}, attempting Cloudflare API overwrite\n`);
      const result = await overwriteDnsRecordForTunnel(hostname, tunnelId, cloudflareAccount);
      process.stdout.write(`[eco proxy] Cloudflare DNS ${result} for ${hostname}\n`);
    } else {
      throw error;
    }
  }
  await pctExec(
    ctid,
    [
      `cat >${configPath} <<'EOF'`,
      `tunnel: ${tunnelId}`,
      `credentials-file: /root/.cloudflared/${tunnelId}.json`,
      "",
      "originRequest:",
      "  noTLSVerify: true",
      "",
      "ingress:",
      `  - hostname: ${hostname}`,
      `    service: ${serviceUrl}`,
      "  - service: http_status:404",
      "EOF"
    ].join("\n")
  );

  await installCtCloudflaredService(ctid, "", serviceName, configPath);
  await pctExec(ctid, `systemctl daemon-reload && systemctl enable ${serviceName} && systemctl restart ${serviceName}`);
  await verifyCloudflaredService(ctid, serviceName);

  return {
    ctid,
    tunnelName: resolvedTunnelName,
    tunnelId,
    configPath,
    serviceName
  };
}

async function runInitTunnel(positionals, options) {
  const target = positionals[0] || "proxy";
  const hostname = positionals[1];
  const result = await ensureProxyTunnel({
    target,
    hostname,
    tunnelName: options.name,
    serviceUrl: "http://127.0.0.1:80",
    dryRun: Boolean(options["dry-run"]),
    nonInteractive: false,
    cloudflareAccount: options.account
  });

  if (options["dry-run"]) {
    process.stdout.write(`eco proxy init-tunnel plan\n\n`);
    result.plan.forEach((step, index) => {
      process.stdout.write(`${index + 1}. ${step}\n`);
    });
  }
}

async function runTunnelReplicas(positionals, options) {
  const account = positionals[0];
  if (!account) {
    throw new Error(`Missing <account> argument.\n\nUsage: eco proxy tunnel-replicas <account> [count] [--target <ctid>] [--dry-run]`);
  }

  const target = options.target || "proxy";
  const ctid = await resolveCtInput(target);
  await ensureCtRunning(ctid);

  const accountSlug = slugifyTunnelName(account);
  const templateName = `cloudflared-${accountSlug}@.service`;
  const configPath = cloudflaredConfigPathForAccount(account);
  const serviceName = cloudflaredServiceNameForAccount(account);

  if (positionals.length === 1) {
    const currentRaw = await pctExecCapture(
      ctid,
      `systemctl list-units 'cloudflared-${accountSlug}@*' --no-legend --state=active 2>/dev/null | wc -l`
    );
    const current = parseInt(currentRaw.trim()) || 0;
    process.stdout.write(`${account}: ${current} active replica(s) on CT ${ctid}\n`);
    return;
  }

  const countStr = positionals[1];
  const desired = parseInt(countStr, 10);
  if (Number.isNaN(desired) || desired < 0) {
    throw new Error(`Invalid replica count: ${countStr}. Must be a non-negative integer.`);
  }

  if (options["dry-run"]) {
    const currentRaw = await pctExecCapture(
      ctid,
      `systemctl list-units 'cloudflared-${accountSlug}@*' --no-legend --state=active 2>/dev/null | wc -l`
    ).catch(() => "0");
    const current = parseInt(currentRaw.trim()) || 0;

    process.stdout.write(`eco proxy tunnel-replicas plan\n\n`);
    process.stdout.write(`Account: ${account} (service: ${templateName})\n`);
    process.stdout.write(`Target CT: ${ctid}\n`);
    process.stdout.write(`Desired: ${desired} replica(s)\n`);
    process.stdout.write(`Current: ${current} active replica(s)\n\n`);

    if (desired > current) {
      process.stdout.write(`Enable new replicas:\n`);
      for (let i = current + 1; i <= desired; i++) {
        process.stdout.write(`  pct exec ${ctid} -- systemctl enable --now cloudflared-${accountSlug}@${i}\n`);
      }
    } else if (desired < current) {
      process.stdout.write(`Disable removed replicas:\n`);
      for (let i = current; i > desired; i--) {
        process.stdout.write(`  pct exec ${ctid} -- systemctl disable --now cloudflared-${accountSlug}@${i}\n`);
      }
    } else {
      process.stdout.write(`Replica count unchanged. Nothing to do.\n`);
    }
    return;
  }

  const configContent = await pctExecCapture(ctid, `cat ${configPath}`);
  const tokenMatch = configContent.match(/^tunnel:\s*(.+)$/m);
  const token = tokenMatch ? tokenMatch[1].trim() : "";

  if (!token) {
    throw new Error(`Cannot read tunnel token from ${configPath} in CT ${ctid}.`);
  }

  const unitContent = `[Unit]
Description=cloudflared ${account} replica %i
After=network-online.target
Wants=network-online.target

[Service]
TimeoutStartSec=15
Type=notify
ExecStart=/usr/bin/cloudflared --no-autoupdate --config ${configPath} tunnel run --token ${token}
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=multi-user.target
`;

  const tempDir = await mkdtemp(path.join(tmpdir(), "eco-cloudflared-replica-"));
  const filePath = path.join(tempDir, `cloudflared-${accountSlug}@.service`);
  try {
    await writeFile(filePath, unitContent, "utf8");
    await pushFileToCt(ctid, filePath, `/etc/systemd/system/${serviceName}@.service`);
  } finally {
    await rm(tempDir, { recursive: true, force: true });
  }
  await pctExec(ctid, "systemctl daemon-reload");

  const currentRaw = await pctExecCapture(
    ctid,
    `systemctl list-units 'cloudflared-${accountSlug}@*' --no-legend --state=active 2>/dev/null | wc -l`
  );
  const current = parseInt(currentRaw.trim()) || 0;

  if (desired > current) {
    process.stdout.write(`[eco proxy] Scaling ${account} from ${current} to ${desired} replica(s)\n`);
    for (let i = current + 1; i <= desired; i++) {
      await pctExec(ctid, `systemctl enable --now cloudflared-${accountSlug}@${i}`);
    }
  } else if (desired < current) {
    process.stdout.write(`[eco proxy] Scaling ${account} from ${current} to ${desired} replica(s)\n`);
    for (let i = current; i > desired; i--) {
      await pctExec(ctid, `systemctl disable --now cloudflared-${accountSlug}@${i}`);
    }
  } else {
    process.stdout.write(`[eco proxy] ${account} already at ${desired} replica(s)\n`);
  }
}

export async function runProxy(args) {
  const [subcommand, ...rest] = args;

  if (!subcommand || subcommand === "help" || subcommand === "--help" || subcommand === "-h") {
    proxyHelp();
    return;
  }

  if (subcommand === "init-tunnel") {
    const { options, positionals } = parseOptions(rest);
    await runInitTunnel(positionals, options);
    return;
  }

  if (subcommand === "tunnel-replicas") {
    const { options, positionals } = parseOptions(rest);
    await runTunnelReplicas(positionals, options);
    return;
  }

  if (subcommand !== "migrate-cloudflared") {
    throw new Error(`Unknown proxy subcommand: ${subcommand}\n\nRun "eco proxy help" for usage.`);
  }

  const { options, positionals } = parseOptions(rest);
  const target = positionals[0] || "proxy";
  const ctid = await resolveCtInput(target);
  const { configPath, credentialsMode, credentialFiles } = await ensureHostCloudflaredFiles();
  const hostServiceFile = await resolveHostCloudflaredServiceFile();

  const plan = [
    `Validate host cloudflared config at ${configPath}`,
    credentialsMode === "file"
      ? `Validate host cloudflared credentials file ${credentialFiles[0]}`
      : `Use token-based cloudflared config (no separate credentials file)`,
    `Reuse host cloudflared systemd unit from ${hostServiceFile}`,
    `Ensure CT ${ctid} is running`,
    `Prepare /etc/cloudflared, /root/.cloudflared, /etc/systemd/system in CT ${ctid}`,
    `Copy host config into CT ${ctid}: /etc/cloudflared/config.yml`,
    ...credentialFiles.map((file) => `Copy credential into CT ${ctid}: /root/.cloudflared/${path.basename(file)}`),
    `Install cloudflared.service in CT ${ctid}`,
    `Enable and restart cloudflared in CT ${ctid}`,
    `Verify cloudflared status in CT ${ctid}`
  ];

  if (options["stop-host"]) {
    plan.push(`Stop and disable host-level cloudflared after CT ${ctid} starts cleanly`);
  }

  if (options["dry-run"]) {
    process.stdout.write(`eco proxy migrate-cloudflared plan\n\n`);
    plan.forEach((step, index) => {
      process.stdout.write(`${index + 1}. ${step}\n`);
    });
    return;
  }

  process.stdout.write(`[eco proxy] Migrating host cloudflared into CT ${ctid}\n`);
  await ensureCtRunning(ctid);
  await pctExec(ctid, "mkdir -p /etc/cloudflared /root/.cloudflared /etc/systemd/system");
  await pushFileToCt(ctid, configPath, "/etc/cloudflared/config.yml");

  for (const file of credentialFiles) {
    await pushFileToCt(ctid, file, `/root/.cloudflared/${path.basename(file)}`);
  }

  try {
    await pushFileToCt(ctid, hostServiceFile, "/etc/systemd/system/cloudflared.service");
  } finally {
    if (hostServiceFile.startsWith(tmpdir())) {
      await rm(path.dirname(hostServiceFile), { recursive: true, force: true });
    }
  }

  await pctExec(ctid, "systemctl daemon-reload && systemctl enable cloudflared && systemctl restart cloudflared");
  await verifyCloudflaredService(ctid);

  if (options["stop-host"]) {
    process.stdout.write(`[eco proxy] Stopping host-level cloudflared\n`);
    await runCommand("systemctl", ["stop", "cloudflared"]);
    await runCommand("systemctl", ["disable", "cloudflared"]);
  }
}
