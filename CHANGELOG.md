# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/danieleades/sourcery/compare/sourcery-v0.1.0...sourcery-v0.1.1) - 2025-12-19

### Other

- update crate README and metadata ([#20](https://github.com/danieleades/sourcery/pull/20))
- *(deps)* bump actions/checkout from 4 to 6

## [0.1.0](https://github.com/danieleades/sourcery/releases/tag/sourcery-v0.1.0) - 2025-12-16

### Added

- [**breaking**] rename the crate ('event-sourcing' -> 'sourcery') ([#16](https://github.com/danieleades/sourcery/pull/16))
- [**breaking**] add an optional postgres event store implementation ([#15](https://github.com/danieleades/sourcery/pull/15))
- [**breaking**] switch to an async API ([#14](https://github.com/danieleades/sourcery/pull/14))
- [**breaking**] improve ergonomics by using associated generics ([#13](https://github.com/danieleades/sourcery/pull/13))
- [**breaking**] add support for optimistic concurrency ([#12](https://github.com/danieleades/sourcery/pull/12))

### Fixed

- *(ci)* update release job perms ([#17](https://github.com/danieleades/sourcery/pull/17))
- *(ci)* allow codecov job to run from fork PRs ([#6](https://github.com/danieleades/sourcery/pull/6))

### Other

- *(deps)* bump actions/upload-pages-artifact from 3 to 4
- *(deps)* bump actions/configure-pages from 4 to 5
- add docs/ ([#8](https://github.com/danieleades/sourcery/pull/8))
- *(test)* expand test coverage and refactor ([#7](https://github.com/danieleades/sourcery/pull/7))
- *(deps)* bump the patch-updates group across 1 directory with 3 updates
- *(deps)* bump thiserror from 1.0.69 to 2.0.17 ([#2](https://github.com/danieleades/sourcery/pull/2))
- *(ci)* add ci workflows ([#1](https://github.com/danieleades/sourcery/pull/1))
- initial commit
