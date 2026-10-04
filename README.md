# FerrFleet Runner

The process that executes one FerrFleet agent run. It is what
`ghcr.io/ferrlabs/ferrfleet/runner` contains, and it is here in the open because
you are being asked to run it next to your repository and your Claude
credential. "Trust the binary" is not an answer to that.

FerrFleet itself, the control plane that schedules runs and stores their
transcripts, is a hosted service and is not in this repository.

## What it does

Given a run id and a token, it asks the API for the run's configuration, clones
the repository if the run has one, starts the `claude` CLI with the composed
prompt, streams the events back as they happen, and reports the result.

Everything it needs arrives at startup:

| Variable | Required | What |
| --- | --- | --- |
| `FERRFLEET_API_URL` | yes | Base URL of the API. Routes sit at its root. |
| `FERRFLEET_RUN_ID` | yes | The run to execute. |
| `FERRFLEET_RUN_TOKEN` | yes | Scoped to that one run, and expiring with it. |
| `CLAUDE_CODE_OAUTH_TOKEN` | yes | Yours. Without it the agent starts and does nothing. |
| `CLAUDE_ENV_FILE` | no | A credentials file to read instead, as our own pods use. |

It holds no credential of its own. The GitHub token it pushes with is minted
per run by the API, scoped to that run's repository, and fetched at the moment
it is needed; the private key that mints it never leaves the API.

## Using it from GitHub Actions

The action in this repository does both halves: it creates the run against the
API and executes it here.

```yaml
name: review
on: pull_request

jobs:
  ferrfleet:
    runs-on: ubuntu-latest
    steps:
      - uses: FerrLabs/FerrFleet-Runner@v1
        with:
          agent: pr-agent
          token: ${{ secrets.FERRFLEET_ORG_TOKEN }}
          claude-token: ${{ secrets.CLAUDE_CODE_OAUTH_TOKEN }}
```

`token` must be a FerrFleet **organization** token carrying the `agents:run`
scope. A personal token resolves to a human and is stripped of its org before it
reaches an org-scoped route, so it cannot create a run at all.

The agent must be set to an external runner in FerrFleet. If it is not, the API
answers `409` and the action says so rather than failing obscurely: FerrFleet
runs that agent itself, and a second run started here would be a duplicate.

Outputs are `run-id` and `url`, set as soon as the run exists, so a later step
can comment the link on the pull request even when the run itself failed.

### Runners without a Docker daemon

The default runs the published image, which needs a daemon. A hardened
self-hosted runner usually has none: a non-privileged Kubernetes pod has no
socket and no `dind` sidecar, and granting it one means granting root on the
node. `mode: binary` downloads the release binary for the runner's architecture
and executes it directly.

```yaml
      - uses: FerrLabs/FerrFleet-Runner@v1
        with:
          mode: binary
          agent: pr-agent
          token: ${{ secrets.FERRFLEET_ORG_TOKEN }}
          claude-token: ${{ secrets.CLAUDE_CODE_OAUTH_TOKEN }}
```

What the runner has to provide: Linux on x86_64 or aarch64, and `curl`, `git`,
`jq`, `tar` and `sha256sum` on PATH. The step checks all of that before the run
is created, so a missing tool fails the job rather than leaving a claimed run
with nothing executing it. `python3` is only warned about, because agent prompts
are told it is there.

The Claude CLI is installed into `~/.local/bin` unless `claude` is already on
PATH, in which case yours is used as it is. Pin it with `claude-cli-version` if
you want a specific one on a runner that has none.

`version` picks the release: a bare major such as `1` takes the newest release
of that major, matching what `image: ...runner:1` does, and a full `1.2.3` takes
that one exactly.

The agent runs in a fresh `$RUNNER_TEMP/ferrfleet-workdir-XXXXXX` rather than the
`/workdir` of the image, and is told so in its system prompt. The run looks
identical from FerrFleet's side.

What does differ, and it is the thing to weigh: the container was the isolation
boundary, and binary mode removes it. An agent runs under `bypassPermissions`,
so in docker mode "only the working directory is writable" is enforced by the
container, while here the same sentence in the system prompt is advice. The
agent has the job's workspace, `$HOME`, and whatever else that runner holds, and
a self-hosted runner usually holds more than a hosted one. The org token is kept
out of its environment deliberately, and the run token it does get dies with the
run, but nothing stops the agent reading the rest of the machine.

So the choice is not "hardened runner, therefore binary mode". It is a daemon on
the runner, or an agent with the run of it. If both are unacceptable, a
throwaway hosted runner in docker mode is the third option.

### What makes the step fail

The run failing: a non-zero exit from the agent, a refusal from the API, a run
that came back queued rather than started.

Not the agent's findings. A finished run reports `status` and `exit_code` and
nothing structured about what it concluded, so a step that failed on "the
reviewer found something" would have to pattern-match the transcript. That is
the kind of check that passes for months and then quietly stops matching. If you
want FerrFleet to gate a merge today, read `run-id` in a later step and decide
there.

### Running it without the action

```bash
docker run --rm \
  -e FERRFLEET_API_URL -e FERRFLEET_RUN_ID -e FERRFLEET_RUN_TOKEN \
  -e CLAUDE_CODE_OAUTH_TOKEN \
  ghcr.io/ferrlabs/ferrfleet/runner:1
```

Or without a container at all, on any Linux host:

```bash
FERRFLEET_WORKING_DIR=/tmp/ferrfleet-workdir ferrfleet-runner
```

Creating the run first is one HTTP call, covered in
[the external runners guide][guide]. Nothing about this is GitHub-specific:
any CI that can make a request and run a container, or just a binary, works the
same way.

`FERRFLEET_WORKING_DIR` overrides the working directory the API asks for, which
is the `/workdir` of the image. Set it anywhere you can write. It is also what
the agent is told its working directory is, so the two cannot drift apart.

## Two runners on one run

A re-run of a workflow, a retried job, or two workflows watching the same event
can each produce a runner for the same run id. The runner claims the run before
doing anything, and the loser exits having touched nothing. The decision is made
by the API in one conditional statement, because runners cannot see each other.

You do not have to design around it and you cannot switch it off.

## Taking runs from a pool

The modes above execute a run somebody already created. `ferrfleet-runner agent`
works the other way round: it is a long-lived process on a machine you host,
registered to one of your organization's runner pools, that asks FerrFleet for
the next run of an agent pointed at that pool, executes it, and asks again. Every
call goes out from the runner, so it works behind a firewall that only allows
outbound HTTPS. Pools themselves are created in FerrFleet, which hands you the
pool token once.

| Variable | Required | What |
| --- | --- | --- |
| `FERRFLEET_API_URL` | yes | Base URL of the API. |
| `FERRFLEET_POOL_TOKEN` | yes | The pool's `ffrp_...` token. It only opens the lease route. |
| `FERRFLEET_RUNNER_NAME` | no | Label shown on the run page. Defaults to the hostname. |
| `FERRFLEET_WORKING_DIR` | no | Where runs work. Defaults to `ferrfleet-runs` in the temp directory. |
| `ANTHROPIC_API_KEY` | yes | Yours, read by `claude` from this process's environment. |

```bash
docker run -d --restart unless-stopped \
  -e FERRFLEET_API_URL=https://api.ferrfleet.com \
  -e FERRFLEET_POOL_TOKEN -e FERRFLEET_RUNNER_NAME=build-farm-07 \
  -e ANTHROPIC_API_KEY \
  ghcr.io/ferrlabs/ferrfleet/runner:1 agent
```

Use an Anthropic API key from a workspace you set aside for these runs, so its
spend and its rate limits are visible on their own. The Claude credential stays
on your machine: the runner never sends it to FerrFleet, and FerrFleet never asks
for it.

What happens to each run:

1. The runner long-polls `POST /runner-pools/lease` with the pool token. An empty
   answer is followed by the next poll at once; a `429`, a `5xx` or an
   unreachable API by a pause that doubles up to 30 seconds. A `401` means the
   token is wrong, was rotated or its pool was revoked, and the process exits
   with an error saying so.
2. A lease brings a run id and a run token. From there the run goes exactly as an
   external one: claim, configuration, clone, `claude`, events, result. The pool
   token is not used again for that run, and is removed from the environment of
   `claude`, `git` and everything the agent starts.
3. From the lease until the run ends, the runner heartbeats with the run token at
   the interval FerrFleet asks for.
4. When FerrFleet takes the run back (a `410` on the heartbeat because it was
   cancelled or superseded, or a `409` on any route because this runner lost the
   lease), the runner kills `claude`, reports nothing more for that run, and goes
   back to polling.
5. Each run gets its own directory under `FERRFLEET_WORKING_DIR`, removed when
   the run is over, so the next run never clones into a used checkout.

One process executes one run at a time. To run several at once, start several
processes, each with its own working directory.

### Ephemeral runners

`ferrfleet-runner agent --ephemeral` takes one run and exits, so each run starts
on a clean machine and an orchestrator (a Kubernetes Job, an autoscaled VM)
replaces the process. It exits `0` when the run completed successfully or
FerrFleet took it back, and `1` when the run failed, the runner could not execute
it, or the pool token was refused.

### Stopping a runner

On `SIGTERM` or `SIGINT` the runner stops polling. A run in progress is left to
finish, within the run's own timeout, and its heartbeats carry on until then.
Give the process a grace period that matches your agents' timeout. If it is
killed before that, the heartbeats stop and FerrFleet settles the run itself
after the lease lapses: a run not yet claimed goes back to the pool, a claimed
one is marked failed rather than run twice.

## What is in here

`shared/` is the contract: the shapes the API and the runner agree on
(`RunConfig`, `ExecutorEvent`, `Checkout`, `RunResult`). It is public for the
same reason the runner is, and the API consumes this crate rather than a copy.

`runner/` is the process itself. It targets Linux: `workspace.rs` uses
`std::os::unix` to lock down the `GIT_ASKPASS` helper's permissions, so it does
not build on Windows.

## Verifying what you pull

Images are signed with cosign and carry an SBOM. Pin by digest rather than by
tag, and verify before you run:

```bash
cosign verify ghcr.io/ferrlabs/ferrfleet/runner:1 \
  --certificate-identity-regexp '^https://github.com/FerrLabs/FerrFleet-Runner/' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

Every release carries `checksums.txt` over its binaries, and a cosign bundle
over that file. The action checks the hash, which catches a truncated download.
The signature is what catches a swapped asset, and checking it is on you:

```bash
cosign verify-blob checksums.txt \
  --bundle checksums.txt.bundle \
  --certificate-identity-regexp '^https://github.com/FerrLabs/FerrFleet-Runner/' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
sha256sum --check --strict checksums.txt
```

## Building it yourself

```bash
cargo build --release --bin ferrfleet-runner
```

No private registry and no credentials: every dependency is public. That is
deliberate, and worth keeping true.

## Releasing

Tag a full version and push it. The workflow builds, pushes, signs with cosign
and attaches an SBOM, tagging the image `1.2.3`, `1` and `latest`. It also
builds the static musl binaries for x86_64 and aarch64, and publishes a GitHub
release carrying them, their `checksums.txt` and its cosign bundle. That release
is what `mode: binary` downloads, so a version with no release cannot be used
that way.

```bash
git tag -a v1.2.3 -m "..." && git push origin v1.2.3
```

Then repoint the moving major tag, which is what
`uses: FerrLabs/FerrFleet-Runner@v1` resolves against:

```bash
git tag -f v1 && git push -f origin v1
```

That one deliberately does not trigger a release: the workflow matches
`v*.*.*` only, so moving it rebuilds nothing.

## Licence

Apache-2.0. See [LICENSE](LICENSE).
