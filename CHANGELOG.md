# Changelog

All notable changes to `runner` will be documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/).

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
