import { spawn } from "node:child_process";
import { createWriteStream } from "node:fs";
import { chmod, mkdir, stat, unlink, writeFile } from "node:fs/promises";
import { get } from "node:https";
import { join } from "node:path";
import { homedir } from "node:os";

import { parseExpose, readEcompose } from "../lib/ecompose.js";

const toolsDir = join(homedir(), ".eco", "tools");
const K6_VERSION = "0.54.0";

const K6_DOWNLOADS = {
  "linux-x64": {
    url: `https://github.com/grafana/k6/releases/download/v${K6_VERSION}/k6-v${K6_VERSION}-linux-amd64.tar.gz`,
    extract: "k6-v{K6_VERSION}-linux-amd64/k6"
  },
  "darwin-x64": {
    url: `https://github.com/grafana/k6/releases/download/v${K6_VERSION}/k6-v${K6_VERSION}-macos-amd64.zip`,
    extract: "k6-v{K6_VERSION}-macos-amd64/k6"
  },
  "darwin-arm64": {
    url: `https://github.com/grafana/k6/releases/download/v${K6_VERSION}/k6-v${K6_VERSION}-macos-arm64.zip`,
    extract: "k6-v{K6_VERSION}-macos-arm64/k6"
  }
};

function platformKey() {
  return `${process.platform}-${process.arch}`;
}

function k6BinPath() {
  return join(toolsDir, "k6");
}

function parseOptions(args) {
  const options = {
    vus: 100,
    duration: "30s",
    rampUp: "10s",
    hostname: null,
    dryRun: false
  };
  const positionals = [];

  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    if (arg === "--dry-run") {
      options.dryRun = true;
      continue;
    }
    if (!arg.startsWith("--")) {
      positionals.push(arg);
      continue;
    }
    const key = arg.slice(2).replace(/-([a-z])/g, (_, c) => c.toUpperCase());
    const value = args[++i];
    if (!value || value.startsWith("--")) {
      throw new Error(`Missing value for option ${arg}`);
    }
    if (key in options) {
      options[key] = value;
    } else {
      throw new Error(`Unknown option: ${arg}`);
    }
  }

  return { options, positionals };
}

function runCapture(command, args, cwd = process.cwd()) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code !== 0) {
        reject(new Error(stderr || `${command} exited with code ${code}`));
        return;
      }
      resolve(stdout.trim());
    });
  });
}

function downloadFile(url, dest) {
  return new Promise((resolve, reject) => {
    const file = createWriteStream(dest);
    get(url, (response) => {
      if (response.statusCode === 302 || response.statusCode === 301) {
        file.close();
        unlink(dest).catch(() => {});
        downloadFile(response.headers.location, dest).then(resolve, reject);
        return;
      }
      if (response.statusCode !== 200) {
        file.close();
        unlink(dest).catch(() => {});
        reject(new Error(`Download failed: HTTP ${response.statusCode} from ${url}`));
        return;
      }
      response.pipe(file);
      file.on("finish", () => {
        file.close();
        resolve();
      });
    }).on("error", (err) => {
      file.close();
      unlink(dest).catch(() => {});
      reject(err);
    });
  });
}

function k6Script(targetUrl, vus, duration, rampUp) {
  const url = targetUrl.replace(/\/+$/, "");

  return `import http from "k6/http";
import { check, sleep } from "k6";

export const options = {
  stages: [
    { duration: "${rampUp}", target: ${vus} },
    { duration: "${duration}", target: ${vus} },
    { duration: "${rampUp}", target: 0 },
  ],
  thresholds: {
    http_req_duration: ["p(95)<2000"],
    http_req_failed: ["rate<0.05"],
  },
  summaryTimeUnit: "ms",
};

const BASE = "${url}";

export default function () {
  const pageRes = http.get(BASE + "/");
  check(pageRes, {
    "GET / status 2xx": (r) => r.status >= 200 && r.status < 400,
  });

  sleep(2);

  const apiRes = http.get(BASE + "/api");
  check(apiRes, {
    "GET /api request made": (r) => r.status < 500,
  });

  sleep(1);

  const authRes = http.get(BASE + "/auth-api");
  check(authRes, {
    "GET /auth-api request made": (r) => r.status < 500,
  });

  sleep(2);
}

export function handleSummary(data) {
  const m = data.metrics;
  const dr = m["http_req_duration"] ? m["http_req_duration"].values : null;
  const fr = m["http_req_failed"] ? m["http_req_failed"].values : null;
  const ch = m.checks ? m.checks.values : null;
  const summary = {
    timestamp: new Date().toISOString(),
    target_url: BASE,
    virtual_users: ${vus},
    peak_duration_s: "${duration}",
    ramp_up_s: "${rampUp}",
    http_reqs: m.http_reqs ? m.http_reqs.values.count : 0,
    http_req_duration_avg_ms: dr ? dr.avg.toFixed(2) : 0,
    http_req_duration_p95_ms: dr ? dr["p(95)"].toFixed(2) : 0,
    http_req_failed_rate: fr ? fr.rate.toFixed(4) : 0,
    checks_passed: ch ? ch.passes : 0,
    checks_total: ch ? (ch.passes + ch.fails) : 0,
  };
  return {
    "stdout": JSON.stringify(summary, null, 2),
  };
}
`;
}

async function ensureK6() {
  const binPath = k6BinPath();
  try {
    await stat(binPath);
    const version = await runCapture(binPath, ["version"]);
    process.stderr.write(`k6 found: ${version}\n`);
    return binPath;
  } catch {
    process.stderr.write("k6 not found. Provisioning...\n");
  }

  const key = platformKey();
  const download = K6_DOWNLOADS[key];
  if (!download) {
    throw new Error(
      `No k6 binary available for ${process.platform}/${process.arch}. ` +
      `Supported platforms: linux-x64, darwin-x64, darwin-arm64.`
    );
  }

  await mkdir(toolsDir, { recursive: true });

  const ext = download.url.endsWith(".zip") ? ".zip" : ".tar.gz";
  const archivePath = join(toolsDir, `k6-${K6_VERSION}${ext}`);
  process.stderr.write(`Downloading k6 v${K6_VERSION} for ${key}...\n`);

  try {
    await downloadFile(download.url, archivePath);
  } catch (err) {
    try { await unlink(archivePath); } catch {}
    throw new Error(`Failed to download k6: ${err.message}`);
  }

  let extractCmd;
  if (ext === ".zip") {
    extractCmd = ["unzip", "-o", archivePath, "-d", toolsDir];
  } else {
    extractCmd = ["tar", "-xzf", archivePath, "-C", toolsDir];
  }

  try {
    await runCapture(extractCmd[0], extractCmd.slice(1));
  } catch (err) {
    try { await unlink(archivePath); } catch {}
    throw new Error(`Failed to extract k6 archive: ${err.message}`);
  }

  const innerPath = join(toolsDir, download.extract.replace("{K6_VERSION}", K6_VERSION));
  try {
    await stat(innerPath);
  } catch {
    throw new Error(`k6 binary not found at expected path after extraction: ${innerPath}`);
  }

  await runCapture("mv", [innerPath, binPath]);
  await chmod(binPath, 0o755);

  try { await unlink(archivePath); } catch {}

  const version = await runCapture(binPath, ["version"]);
  process.stderr.write(`k6 provisioned: ${version}\n`);
  return binPath;
}

export async function runStress(args) {
  const { options, positionals } = parseOptions(args);

  let input = positionals[0] || ".";
  if (input === "." || input === "./") {
    input = ".";
  }

  let hostname = options.hostname;
  let filePath = "";

  if (!hostname) {
    const ecompose = await readEcompose(input);
    filePath = ecompose.filePath;
    const expose = parseExpose(ecompose.content);
    hostname = expose.hostname;
  }

  if (!hostname) {
    throw new Error(
      "No hostname found. Set expose.hostname in ecompose.yml or pass --hostname."
    );
  }

  const targetUrl = `https://${hostname}`;

  if (options.dryRun) {
    const lines = [
      `Target:    ${targetUrl}`,
      `VUs:       ${options.vus}`,
      `Ramp-up:   ${options.rampUp}`,
      `Duration:  ${options.duration}`,
      `k6 bin:    ${k6BinPath()}`,
    ];
    if (filePath) {
      lines.push(`Manifest:  ${filePath}`);
    }
    process.stdout.write(lines.join("\n") + "\n");
    return;
  }

  process.stderr.write(`Stress-testing ${targetUrl}
  VUs: ${options.vus}  ramp-up: ${options.rampUp}  duration: ${options.duration}

`);

  const k6 = await ensureK6();

  const scriptPath = join(toolsDir, "stress-test.js");
  await writeFile(scriptPath, k6Script(targetUrl, options.vus, options.duration, options.rampUp));

  const k6Args = ["run", "--quiet", scriptPath];

  process.stderr.write("Running k6...\n\n");

  await new Promise((resolve, reject) => {
    const child = spawn(k6, k6Args, { stdio: "inherit" });
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code !== 0 && code !== null) {
        reject(new Error(`k6 exited with code ${code}`));
        return;
      }
      resolve();
    });
  });
}
