# CI/CD Direction

## Recommendation

For the current `eco + Proxmox + CT` approach, the best first CI/CD setup is:

1. `GitHub Actions` with a `self-hosted runner`
2. Runner installed in its own dedicated Proxmox CT, for example `ci`
3. Deployment executed through `eco`, not through ad-hoc shell scripts spread across repositories

Do not start with Jenkins unless there is a clear need for its extra complexity.

## Why

### GitHub Actions + self-hosted runner

- Best fit if repositories stay on GitHub
- Simple to adopt incrementally
- Works well with the current Proxmox model
- Keeps CI isolated from the Proxmox host by placing the runner in its own CT
- Lets deployments target other CTs or the Proxmox host through controlled access

### Gitea Actions later

- Best future option if git hosting is later self-hosted
- Conceptually close to GitHub Actions
- Likely easier migration path than moving from Jenkins
- Fits the long-term `eco` ecosystem direction better than Jenkins

### Jenkins

- Powerful, but operationally heavier
- Plugin sprawl and maintenance burden are real
- Not the right first move for this stage

## Proposed Architecture

### 1. Dedicated CI CT

Create a dedicated CT such as:

- `ci`

Its purpose:

- run repository pipelines
- perform build/test steps
- trigger deployments

This keeps CI tooling out of:

- the Proxmox host
- application CTs

### 2. Deployment Through eco

The deployment logic should live behind an `eco` command, for example:

- `eco deploy`

Not:

- custom shell sequences typed manually each time
- CI pipelines containing all deployment logic inline

CI should call `eco`.

## Desired Flow

On push to `main`:

1. checkout code
2. install dependencies
3. run tests
4. run build
5. if all pass, deploy
6. run health check
7. if health check fails, stop and later support rollback

## Deploy Shape

For the current stack, deployment should typically do:

1. `git pull`
2. install dependencies only when needed
3. build production artifacts
4. restart PM2 services
5. run health checks

Example health checks:

- frontend route returns `200`
- auth backend responds
- assessment backend responds

## Secrets and Access

The CI CT should hold only the minimum needed secrets, such as:

- repository access
- deployment SSH key
- optional API tokens

Preferred access model:

- CI runner in CT
- CI runner connects to deployment targets with controlled SSH

Avoid putting more tools than necessary directly on the Proxmox host.

## Practical Path

### Stage 1

- Use `GitHub Actions`
- Add one self-hosted runner in a dedicated `ci` CT
- Add one deployment workflow for `main`

### Stage 2

- Add `eco deploy`
- Move repo-specific deploy steps behind `eco`
- Standardize build/test/deploy conventions across projects

### Stage 3

- Add health checks and failure handling
- Add environment separation if needed
- Add reusable workflow templates

### Stage 4

- If git hosting becomes self-hosted, migrate to `Gitea Actions`
- Keep the same overall workflow shape
- Keep `eco` as the deployment interface

## Current Conclusion

If implemented now, the recommended first CI/CD stack is:

- GitHub Actions
- self-hosted runner
- runner isolated inside its own Proxmox CT
- deployment performed by `eco`

Not recommended for the first implementation:

- Jenkins

## Notes

This should be tackled later, after the current manual deployment path is stable enough that CI/CD will automate a known-good process rather than automate instability.
