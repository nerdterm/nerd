# Changelog

## [0.13.0](https://github.com/nerdterm/nerd/compare/v0.12.4...v0.13.0) (2026-10-20)


### Added

* Control + B is nerd's key by default; terminal presets for Option, margin and colours ([#83](https://github.com/nerdterm/nerd/issues/83)) ([850c2c1](https://github.com/nerdterm/nerd/commit/850c2c1aefe14e7064c8c6a913686bb215f1eb23))
* Isolation chooses which of an agent's providers a pane may reach ([#73](https://github.com/nerdterm/nerd/issues/73)) ([d210646](https://github.com/nerdterm/nerd/commit/d210646b6dd103654d0d3d48df8f83e16f03c340))


### Fixed

* publishing a release rebuilds nerdterm.dev right away ([#71](https://github.com/nerdterm/nerd/issues/71)) ([7372d31](https://github.com/nerdterm/nerd/commit/7372d31a8b446de1afdd542ed206fa4526785c3e))
* the sidebar logo is drawn from a raster near the terminal's row height ([#82](https://github.com/nerdterm/nerd/issues/82)) ([9ffc957](https://github.com/nerdterm/nerd/commit/9ffc957a5ee281bbde1dcd34c2c8872b180f09db))

## [0.12.4](https://github.com/nerdterm/nerd/compare/v0.12.3...v0.12.4) (2026-10-18)


### Fixed

* a message an agent has taken no longer counts as unread ([#61](https://github.com/nerdterm/nerd/issues/61)) ([2680e48](https://github.com/nerdterm/nerd/commit/2680e48e2ac7b883cb643771fd657a6ce48f582d))
* a restarted pane also drops the old program's scroll region, origin, insert and charset ([#68](https://github.com/nerdterm/nerd/issues/68)) ([7bc0f18](https://github.com/nerdterm/nerd/commit/7bc0f18f95d69abfa46656d6a6a30e5f6c736039))
* pi reaches every provider it ships with in an Only these providers pane ([#62](https://github.com/nerdterm/nerd/issues/62)) ([4e8a5e1](https://github.com/nerdterm/nerd/commit/4e8a5e136f00fc93e8722617890741eb4e5fe60f))
* pi starts with its models in a pane set to this folder only ([#63](https://github.com/nerdterm/nerd/issues/63)) ([e92d5fb](https://github.com/nerdterm/nerd/commit/e92d5fbb0fff30cfa285941f573fcfdfc648eaac))
* pi's provider list leaves out the Cloudflare hosts that relay any URL ([#69](https://github.com/nerdterm/nerd/issues/69)) ([17d833c](https://github.com/nerdterm/nerd/commit/17d833c328befd167be5b5808e13364fccd89749))
* restarting a pane's program starts its agent again with clean terminal modes ([#64](https://github.com/nerdterm/nerd/issues/64)) ([c1e398e](https://github.com/nerdterm/nerd/commit/c1e398e59808c6435d8fa53a9d578807987486ad))

## [0.12.3](https://github.com/nerdterm/nerd/compare/v0.12.2...v0.12.3) (2026-10-17)


### Fixed

* rows in needs you keep their order when agents finish at the same moment ([#54](https://github.com/nerdterm/nerd/issues/54)) ([b22df0b](https://github.com/nerdterm/nerd/commit/b22df0b40841da01239b80530c31a3f051d32d4c))

## [0.12.2](https://github.com/nerdterm/nerd/compare/v0.12.1...v0.12.2) (2026-10-17)


### Added

* nerd installs with curl -fsSL https://nerdterm.dev/install.sh | sh ([#50](https://github.com/nerdterm/nerd/issues/50)) ([6c48b79](https://github.com/nerdterm/nerd/commit/6c48b79e63bee6f82b2507ca7d9ea524fe1724bd))

## [0.12.1](https://github.com/nerdterm/nerd/compare/v0.12.0...v0.12.1) (2026-10-17)


### Fixed

* a y/N answer with a byte that is not UTF-8 no longer stops nerd --install-hook ([#42](https://github.com/nerdterm/nerd/issues/42)) ([97915bb](https://github.com/nerdterm/nerd/commit/97915bbaf600f92a30c57f15e3de642f1fda8feb))

## [0.12.0](https://github.com/nerdterm/nerd/compare/v0.11.0...v0.12.0) (2026-10-17)


### Added

* nerd in its public repository ([5ecc6b3](https://github.com/nerdterm/nerd/commit/5ecc6b3a302202211883062b0ff94e9de156d949))

## Changelog
