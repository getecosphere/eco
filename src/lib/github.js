function normalizeRepoPath(raw) {
  if (!raw) {
    return "";
  }
  return raw
    .replace(/^ssh:\/\//, "")
    .replace(/^git@[^:]+:/, "")
    .replace(/^https?:\/\/[^/]+\//, "")
    .replace(/^\/+/, "")
    .replace(/\.git$/, "");
}

export function parseGithubRepoCoordinates(remoteUrl) {
  const normalized = normalizeRepoPath(remoteUrl);
  const match = normalized.match(/^([^/]+)\/([^/]+)$/);
  if (!match) {
    throw new Error(`Cannot parse GitHub repo coordinates from remote URL: ${remoteUrl}`);
  }

  return {
    owner: match[1],
    repo: match[2],
    fullName: `${match[1]}/${match[2]}`
  };
}

async function githubRequest(pathname, { token, method = "GET", body } = {}) {
  if (!token) {
    throw new Error("Missing GITHUB_TOKEN.");
  }

  const response = await fetch(`https://api.github.com${pathname}`, {
    method,
    headers: {
      Accept: "application/vnd.github+json",
      Authorization: `Bearer ${token}`,
      "User-Agent": "eco-cli",
      "X-GitHub-Api-Version": "2022-11-28",
      ...(body ? { "Content-Type": "application/json" } : {})
    },
    body: body ? JSON.stringify(body) : undefined
  });

  if (response.status === 204) {
    return null;
  }

  const text = await response.text();
  const payload = text ? JSON.parse(text) : null;
  if (!response.ok) {
    const message = payload?.message || text || `GitHub API request failed: ${response.status}`;
    throw new Error(message);
  }
  return payload;
}

export async function syncGithubPushWebhook({
  token,
  owner,
  repo,
  webhookUrl,
  secret,
  staleWebhookHostname = ""
}) {
  const hooks = await githubRequest(`/repos/${owner}/${repo}/hooks`, { token });
  const list = Array.isArray(hooks) ? hooks : [];
  const existing = list.find((hook) => hook?.config?.url === webhookUrl);

  // The webhook hostname is derived from the estate's hostname. Older eco
  // versions derived a nested two-level form (`hooks.eco.stuff8.com`) that
  // Cloudflare Universal SSL does not cover (TLS handshake failure). Purge
  // any hook still pointing at that legacy hostname so a re-run of eco up
  // replaces the broken hook instead of leaving it failing alongside the
  // new one.
  const stale = staleWebhookHostname
    ? list.filter((hook) => hook?.config?.url === `https://${staleWebhookHostname}${new URL(webhookUrl).pathname}`)
    : [];
  for (const hook of stale) {
    await githubRequest(`/repos/${owner}/${repo}/hooks/${hook.id}`, {
      token,
      method: "DELETE"
    });
  }

  const body = {
    active: true,
    events: ["push"],
    config: {
      url: webhookUrl,
      content_type: "json",
      secret,
      insecure_ssl: "0"
    }
  };

  if (existing?.id) {
    await githubRequest(`/repos/${owner}/${repo}/hooks/${existing.id}`, {
      token,
      method: "PATCH",
      body
    });
    return { action: "updated", hookId: existing.id, removedStale: stale.length };
  }

  const created = await githubRequest(`/repos/${owner}/${repo}/hooks`, {
    token,
    method: "POST",
    body
  });

  return { action: "created", hookId: created?.id || "", removedStale: stale.length };
}
