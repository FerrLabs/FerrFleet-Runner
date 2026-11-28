# Changelog

All notable changes to `runner` will be documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/).

## [1.15.1] - 2026-11-28

### Bug Fixes

- fix(runner): report the git error when workspace preparation fails (#158)

## [1.15.0] - 2026-11-27

### Features

- feat(runner): stream thinking and partial messages as deltas (#153)

## [1.14.2] - 2026-11-27

### Bug Fixes

- fix(runner): kill every descendant of a stopped run, not only its process group (#156)

## [1.14.1] - 2026-11-27

### Bug Fixes

- fix(fuzz): declare the time dependency so the targets build (#154)

## [1.14.0] - 2026-11-27

### Features

- feat(events): add Thinking and Delta run events (#152)

## [1.13.3] - 2026-11-26

### Bug Fixes

- fix(runner): kill everything claude started when a run ends (#147)

## [1.13.2] - 2026-11-26

### Bug Fixes

- fix(deps): update url to 2.5.9 (RUSTSEC-2026-0592) (#148)
- fix(runner): keep the prepared branch and surface a refused pull-request registration (#141)

## [1.13.1] - 2026-11-18

### Bug Fixes

- fix(deps): replace unmaintained chrono with time (RUSTSEC-2026-0543) (#139)

## [1.13.0] - 2026-11-16

### Features

- feat(runner): pass the run effort to the claude CLI (#136)

## [1.12.0] - 2026-11-15

### Features

- feat(runner): report CPU and memory usage in the heartbeat (#133)

## [1.11.0] - 2026-11-15

### Features

- feat(formal): persistent build cache for the formal image (#132)

## [1.10.0] - 2026-11-11

### Features

- feat(runner): refuse stdio MCP servers on managed runs (#128)

## [1.9.0] - 2026-11-11

### Features

- feat(image): ship a pinned mcp-grafana (#127)

### Bug Fixes

- fix(runner): clear stale run directories when the pool agent starts (#123)

## [1.8.0] - 2026-11-10

### Features

- feat(formal): add sccache and extra volumes for a persistent build cache (#124)

## [1.7.2] - 2026-11-10

### Bug Fixes

- fix(runner): never offer tools that wait for a later turn (#120)

## [1.7.1] - 2026-11-09

### Bug Fixes

- fix(formal): run cargo and Kani on a read-only root with HOME=/tmp (#117)

## [1.7.0] - 2026-11-09

### Features

- feat(formal): ship the ferrflow CLI in the formal runner image (#116)

### Bug Fixes

- fix(formal): leave 4GiB of the pod to everything but cbmc (#113)
- fix(action): verify the cached runner archive and cache only exact CLI versions (#108)
- perf(action): cache the runner and Claude CLI downloads and disable the CLI autoupdater (#106)

### Refactoring

- refactor(runner): split main.rs into repo_context, claude and subcommands modules (#107)
