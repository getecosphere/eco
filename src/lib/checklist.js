import readline from "node:readline";
import { stdin as input, stdout as output } from "node:process";

// Shared interactive checklist/confirm UI -- originally only in
// startproject.js, extracted so `eco compose add` can offer the same
// arrow-key multi-select experience for picking repos to compose into an
// existing estate, instead of requiring a single positional argument.

export function ensureInteractive() {
  if (!input.isTTY || !output.isTTY) {
    throw new Error("This command requires an interactive terminal.");
  }
}

export function withRawMode(fn) {
  ensureInteractive();
  readline.emitKeypressEvents(input);
  if (typeof input.resume === "function") {
    input.resume();
  }
  if (typeof input.setRawMode === "function") {
    input.setRawMode(true);
  }

  return Promise.resolve()
    .then(fn)
    .finally(() => {
      if (typeof input.setRawMode === "function") {
        input.setRawMode(false);
      }
      if (typeof input.pause === "function") {
        input.pause();
      }
    });
}

// requiresByRepo: repoName -> [dependency names]
// requiredByRepo: repoName -> [names of repos that require it]
export function buildRepoDependencyMaps(repoCatalog) {
  const requiresByRepo = new Map();
  const requiredByRepo = new Map();

  for (const repo of repoCatalog) {
    requiresByRepo.set(repo.name, Array.isArray(repo.requires) ? [...repo.requires] : []);
    if (!requiredByRepo.has(repo.name)) {
      requiredByRepo.set(repo.name, []);
    }
  }

  for (const repo of repoCatalog) {
    for (const dependency of requiresByRepo.get(repo.name) || []) {
      if (!requiredByRepo.has(dependency)) {
        requiredByRepo.set(dependency, []);
      }
      requiredByRepo.get(dependency).push(repo.name);
    }
  }

  return { requiresByRepo, requiredByRepo };
}

export function collectDependencies(repoName, requiresByRepo, into = new Set()) {
  for (const dependency of requiresByRepo.get(repoName) || []) {
    if (!into.has(dependency)) {
      into.add(dependency);
      collectDependencies(dependency, requiresByRepo, into);
    }
  }
  return into;
}

export function computeLockedRepos(selected, requiredByRepo) {
  const locked = new Set();
  for (const repoName of selected) {
    const dependents = requiredByRepo.get(repoName) || [];
    if (dependents.some((dependent) => selected.has(dependent))) {
      locked.add(repoName);
    }
  }
  return locked;
}

function renderChecklist(title, items, cursor, selected, locked, hint) {
  const lines = ["", title, hint, ""];
  items.forEach((item, index) => {
    const pointer = index === cursor ? "❯" : " ";
    const mark = selected.has(item.id) ? "x" : " ";
    const suffix = locked.has(item.id) ? " [required]" : "";
    lines.push(` ${pointer} [${mark}] ${item.label}${suffix}`);
  });
  return lines.join("\n");
}

// items: [{id, label}]. requiresByRepo/requiredByRepo (both optional): when
// given, selecting an item auto-selects its dependency closure and locks
// those from being unselected while still required, same as startproject's
// repo picker. Without them, items behave as plain independent checkboxes.
export async function runChecklist({
  items,
  title,
  hint,
  requiresByRepo,
  requiredByRepo,
  minSelected = 1,
  initialSelected = [],
  lockedIds = []
}) {
  let cursor = 0;
  const selected = new Set(initialSelected);
  const permanentlyLocked = new Set(lockedIds);
  let error = "";

  return withRawMode(() => new Promise((resolve, reject) => {
    function paint() {
      const locked = new Set([
        ...permanentlyLocked,
        ...(requiredByRepo ? computeLockedRepos(selected, requiredByRepo) : [])
      ]);
      readline.cursorTo(output, 0, 0);
      readline.clearScreenDown(output);
      output.write(renderChecklist(title, items, cursor, selected, locked, hint));
      if (error) {
        output.write(`\n\n${error}`);
      }
    }

    function cleanup() {
      input.removeListener("keypress", onKeypress);
      output.write("\n");
    }

    function onKeypress(str, key) {
      if (key.name === "up") {
        cursor = cursor === 0 ? items.length - 1 : cursor - 1;
        error = "";
        paint();
        return;
      }

      if (key.name === "down") {
        cursor = cursor === items.length - 1 ? 0 : cursor + 1;
        error = "";
        paint();
        return;
      }

      if (key.name === "space" || str === "x" || str === "X") {
        const id = items[cursor].id;
        const locked = new Set([
          ...permanentlyLocked,
          ...(requiredByRepo ? computeLockedRepos(selected, requiredByRepo) : [])
        ]);

        if (selected.has(id)) {
          if (locked.has(id)) {
            error = `${id} is required by another selected item and cannot be unselected.`;
            paint();
            return;
          }
          selected.delete(id);
        } else {
          selected.add(id);
          if (requiresByRepo) {
            collectDependencies(id, requiresByRepo).forEach((dependency) => selected.add(dependency));
          }
        }

        error = "";
        paint();
        return;
      }

      if (key.name === "return") {
        const result = items.filter((item) => selected.has(item.id)).map((item) => item.id);
        if (result.length < minSelected) {
          error = `At least ${minSelected} item${minSelected === 1 ? "" : "s"} must be selected.`;
          paint();
          return;
        }
        cleanup();
        resolve(result);
        return;
      }

      if (key.name === "c" && key.ctrl) {
        cleanup();
        reject(new Error("Cancelled."));
      }
    }

    input.on("keypress", onKeypress);
    paint();
  }));
}

export async function confirmWithSingleKey(message, { defaultYes = false } = {}) {
  return withRawMode(() => new Promise((resolve, reject) => {
    output.write(`${message} [${defaultYes ? "Y/n" : "y/N"}] (Enter = ${defaultYes ? "yes" : "no"}): `);

    function cleanup(answer) {
      input.removeListener("keypress", onKeypress);
      output.write(`${answer}\n`);
    }

    function onKeypress(str, key) {
      const lower = String(str || "").toLowerCase();
      if (lower === "y") {
        cleanup("y");
        resolve(true);
        return;
      }
      if (lower === "n") {
        cleanup("n");
        resolve(false);
        return;
      }
      if (key.name === "return") {
        cleanup(defaultYes ? "y" : "n");
        resolve(defaultYes);
        return;
      }
      if (key.name === "c" && key.ctrl) {
        cleanup("");
        reject(new Error("Cancelled."));
      }
    }

    input.on("keypress", onKeypress);
  }));
}
