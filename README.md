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

## Running one

```bash
docker run --rm \
  -e FERRFLEET_API_URL -e FERRFLEET_RUN_ID -e FERRFLEET_RUN_TOKEN \
  -e CLAUDE_CODE_OAUTH_TOKEN \
  ghcr.io/ferrlabs/ferrfleet/runner:1
```

Creating the run first, and wiring this into GitHub Actions or any other CI, is
covered in [the external runners guide][guide].

[guide]: https://github.com/FerrLabs/FerrFleet-Cloud/blob/main/docs/external-runners.md

## Two runners on one run

A re-run of a workflow, a retried job, or two workflows watching the same event
can each produce a runner for the same run id. The runner claims the run before
doing anything, and the loser exits having touched nothing. The decision is made
by the API in one conditional statement, because runners cannot see each other.

You do not have to design around it and you cannot switch it off.

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

## Building it yourself

```bash
cargo build --release --bin ferrfleet-runner
```

No private registry and no credentials: every dependency is public. That is
deliberate, and worth keeping true.

## Licence

Apache-2.0. See [LICENSE](LICENSE).
