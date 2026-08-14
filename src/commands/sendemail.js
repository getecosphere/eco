import { spawn } from "node:child_process";

function shell(sshHost, cmd, opts = {}) {
  return new Promise((resolve, reject) => {
    const args = [sshHost, cmd];
    const child = spawn("ssh", args, {
      stdio: [opts.stdin || "ignore", "pipe", opts.stderr || "pipe"],
    });
    let out = "";
    let err = "";
    child.stdout.on("data", (d) => { out += d.toString(); });
    child.stderr.on("data", (d) => { err += d.toString(); });
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code !== 0) reject(new Error(`ssh exited ${code}: ${err}`));
      else resolve(out.trim());
    });
  });
}

async function resolveBrevoCredential(keyName) {
  const cmd = `bash -lc 'echo \${${keyName}}'`;
  return shell("prox", cmd);
}

function helpText() {
  process.stdout.write(`eco sendemail

Usage:
  eco sendemail --to <recipient> --subject <text> [--html <html>] [--host <alias>]
  eco sendemail --to <recipient> --subject <text> --file <path>      (read html from file)
  echo "<html>" | eco sendemail --to <recipient> --subject <text>    (read html from stdin)

Sends a transactional email via Brevo using the Proxmox host's credentials
(BREVO_API_KEY, MAIL_FROM_EMAIL, MAIL_FROM_NAME from /root/.bashrc).

Options:
  --to <email>        Recipient email address (required)
  --subject <text>    Email subject (required)
  --html <html>       HTML body (if not using --file or stdin)
  --file <path>       Read HTML body from a local file
  --host <hostname>   SSH host for the Proxmox host (default: prox)

Defaults when called from AI context:
  --to defaults to kampusrwids@gmail.com when the user says "send email to me"
  --from uses MAIL_FROM_EMAIL from the host (no-reply@jogjaitcamp.com)

Examples:
  eco sendemail --to dev@example.com --subject "Deploy done" --html "<p>OK</p>"
  eco sendemail --to user@test.com --subject "Report" --file ./report.html
  echo "<h1>Hello</h1>" | eco sendemail --to me@test.com --subject "Status"
`);
}

export async function runSendemail(args) {
  if (args[0] === "help" || args[0] === "--help" || args[0] === "-h") {
    helpText();
    return;
  }

  const options = {};
  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    if (!arg.startsWith("--")) throw new Error(`Unexpected argument: ${arg}`);
    const key = arg.slice(2);
    const value = args[i + 1];
    if (!value || value.startsWith("--")) throw new Error(`Missing value for option --${key}`);
    options[key] = value;
    i += 1;
  }

  if (!options.to || !options.subject) {
    throw new Error("Both --to and --subject are required. Run `eco sendemail help` for usage.");
  }

  let html = options.html || "";

  if (options.file) {
    const { readFileSync } = await import("node:fs");
    html = readFileSync(options.file, "utf8");
  }

  if (!html && !process.stdin.isTTY) {
    const chunks = [];
    for await (const chunk of process.stdin) {
      chunks.push(chunk);
    }
    html = Buffer.concat(chunks).toString("utf8");
  }

  if (!html) {
    throw new Error("No HTML body provided. Use --html, --file, or pipe content via stdin.");
  }

  const host = options.host || "prox";

  process.stdout.write(`Sending email to ${options.to}…\n`);

  const apiKey = await resolveBrevoCredential("BREVO_API_KEY");
  const fromEmail = await resolveBrevoCredential("MAIL_FROM_EMAIL");
  const fromName = await resolveBrevoCredential("MAIL_FROM_NAME");

  if (!apiKey) throw new Error("BREVO_API_KEY is not set on the Proxmox host.");
  if (!fromEmail) throw new Error("MAIL_FROM_EMAIL is not set on the Proxmox host.");

  const payload = JSON.stringify({
    sender: { name: fromName || "Eco", email: fromEmail },
    to: [{ email: options.to }],
    subject: options.subject,
    htmlContent: html,
  });

  const curlCmd = `curl -s -w '\\n%{http_code}' -X POST https://api.brevo.com/v3/smtp/email ` +
    `-H 'api-key: ${apiKey}' ` +
    `-H 'Content-Type: application/json' ` +
    `-d '${payload.replace(/'/g, "'\\''")}'`;

  const raw = await shell(host, curlCmd);
  const lines = raw.split("\n");
  const statusCode = lines[lines.length - 1].trim();
  const body = lines.slice(0, -1).join("\n").trim();

  if (statusCode === "201" || statusCode === "202") {
    let messageId = "";
    try { messageId = JSON.parse(body).messageId; } catch {}
    process.stdout.write(`Email sent successfully${messageId ? ` (messageId: ${messageId})` : ""}.\n`);
  } else {
    throw new Error(`Brevo returned HTTP ${statusCode}: ${body}`);
  }
}
