# Changelog

## [0.19.1](https://github.com/nerdterm/nerd/compare/v0.19.0...v0.19.1) (2026-10-28)


### Fixed

* a preview picture goes while the notice card is drawn over it ([#169](https://github.com/nerdterm/nerd/issues/169)) ([6ca3a7f](https://github.com/nerdterm/nerd/commit/6ca3a7f7d784b306fdd81b9daa13e831c6a0f6bb))
* Servers says why a machine cannot be listed, and offers a terminal only when ssh wanted a prompt ([#171](https://github.com/nerdterm/nerd/issues/171)) ([5519b1e](https://github.com/nerdterm/nerd/commit/5519b1e3fe9eec84b72518f3c2b9e17e852f7bf4))

## [0.19.0](https://github.com/nerdterm/nerd/compare/v0.18.1...v0.19.0) (2026-10-28)


### Added

* a PDF previews as its first page, with its page count ([#167](https://github.com/nerdterm/nerd/issues/167)) ([cb8322d](https://github.com/nerdterm/nerd/commit/cb8322de43dae7d5061937e5edd9f9006ec8494a))
* a tab nobody named shows what runs in it, and a space holds nine tabs ([#168](https://github.com/nerdterm/nerd/issues/168)) ([8b9f799](https://github.com/nerdterm/nerd/commit/8b9f799816b38a440623cd429ea7c2addc6b8a30))
* an SVG file previews as its picture, and a click in the header shows its source ([#164](https://github.com/nerdterm/nerd/issues/164)) ([9af1131](https://github.com/nerdterm/nerd/commit/9af1131fad8c1ee74991e16dde002202d363dfaa))
* review an agent's changes and send it comments on their lines ([#162](https://github.com/nerdterm/nerd/issues/162)) ([626797a](https://github.com/nerdterm/nerd/commit/626797a85a4b95aef8133a4b414a6cbfed505237))


### Fixed

* every click target shows the hand, the New space path the text cursor, and a picture goes while a window is open ([#166](https://github.com/nerdterm/nerd/issues/166)) ([e2838bd](https://github.com/nerdterm/nerd/commit/e2838bd26ae375f5386846ac2421c85d49df9f48))

## [0.18.1](https://github.com/nerdterm/nerd/compare/v0.18.0...v0.18.1) (2026-10-26)


### Fixed

* a Homebrew install is told to run brew update && brew upgrade nerd ([#159](https://github.com/nerdterm/nerd/issues/159)) ([b93f190](https://github.com/nerdterm/nerd/commit/b93f1901642b12ec2df8b5b917d13d04dd324194))
* a pane stops asking you as soon as its question is answered, not when the allowed action ends ([#158](https://github.com/nerdterm/nerd/issues/158)) ([e4d73c5](https://github.com/nerdterm/nerd/commit/e4d73c58b6c77ce53a555c908fe6224ddc14985f))

## [0.18.0](https://github.com/nerdterm/nerd/compare/v0.17.1...v0.18.0) (2026-10-26)


### Added

* an agent learns whom it may write to changed, only in its own commands' output ([#143](https://github.com/nerdterm/nerd/issues/143)) ([14c7f8f](https://github.com/nerdterm/nerd/commit/14c7f8f777eb8eed875042029b5788ea7c4a44c7))
* install.sh warns when a folder above nerd can be written by others ([#150](https://github.com/nerdterm/nerd/issues/150)) ([b1371bd](https://github.com/nerdterm/nerd/commit/b1371bda128acc40162a3638f1c72c62db9f36f0))
* the Messages matrix shows spaces first and opens a pair of spaces on a press ([#147](https://github.com/nerdterm/nerd/issues/147)) ([f9bccf9](https://github.com/nerdterm/nerd/commit/f9bccf9015b199ff8cf75a0a672072705362b9e5))


### Fixed

* a needs-you row names the pane whose agent waits ([#152](https://github.com/nerdterm/nerd/issues/152)) ([e59447d](https://github.com/nerdterm/nerd/commit/e59447d7a583bf5389642e7f706c70cb600549ae))
* a pane under heavy load waits for a live broker, and titles keep following processes when ps is slow ([#153](https://github.com/nerdterm/nerd/issues/153)) ([a475310](https://github.com/nerdterm/nerd/commit/a475310d3a63caa20e1b5c1cbff2005f9b739efc))
* a process outside nerd is not told to try again while a pane starts ([#138](https://github.com/nerdterm/nerd/issues/138)) ([f97eaa8](https://github.com/nerdterm/nerd/commit/f97eaa874b73ce0ceb7b073bcb16f80224119519))
* a sender outside the pane it names is told why it cannot send ([#136](https://github.com/nerdterm/nerd/issues/136)) ([1a6192e](https://github.com/nerdterm/nerd/commit/1a6192edb00140b2a7a8be64ff6e0dbadb8484c5))
* an agent pane without its hook is not done on every redraw or pause ([#155](https://github.com/nerdterm/nerd/issues/155)) ([9c8ad72](https://github.com/nerdterm/nerd/commit/9c8ad7258840f82af64ad65f2e1955040c9f617e))
* every ability in the plugin install and update question is shown whole, wrapped under its own start ([#151](https://github.com/nerdterm/nerd/issues/151)) ([4a538d3](https://github.com/nerdterm/nerd/commit/4a538d3a4fc177268bcd9aacbccbd3e270f4ab7f))
* text from the socket or another agent can neither drive a terminal nor hide a forged frame ([#142](https://github.com/nerdterm/nerd/issues/142)) ([af1e606](https://github.com/nerdterm/nerd/commit/af1e6069fd8c93536b632916579a8f7d5c6998d7))
* the New space field has a caret that keys and clicks move, and a pasted path replaces the prefilled ~/ ([#144](https://github.com/nerdterm/nerd/issues/144)) ([c87d53c](https://github.com/nerdterm/nerd/commit/c87d53c9eca3521ea3301566f864c71ab3e10941))
* two panes answering each other are not held after eight turns, and allow-for-an-hour lets them go on ([#148](https://github.com/nerdterm/nerd/issues/148)) ([5d7ac9c](https://github.com/nerdterm/nerd/commit/5d7ac9cae0757f280ff38259bad8c2d5ad782da4))

## [0.17.1](https://github.com/nerdterm/nerd/compare/v0.17.0...v0.17.1) (2026-10-24)


### Fixed

* each window type has a fixed width and scrolls when too tall ([#130](https://github.com/nerdterm/nerd/issues/130)) ([fa0d437](https://github.com/nerdterm/nerd/commit/fa0d4376f922addb21ef84217a6f358caf910c35))
* no server name beside the logo ([#127](https://github.com/nerdterm/nerd/issues/127)) ([4b6e769](https://github.com/nerdterm/nerd/commit/4b6e769261113d028ae5654b797a385af51a6b42))
* the menu button's badge ends on the border lines and its strokes never move ([#126](https://github.com/nerdterm/nerd/issues/126)) ([8600ac0](https://github.com/nerdterm/nerd/commit/8600ac02251ccef144a08e0f50802391d7df0af8))
* the terminal gets its cursor back when nerd's screen goes down ([#121](https://github.com/nerdterm/nerd/issues/121)) ([566a809](https://github.com/nerdterm/nerd/commit/566a809342ca0b03e0becdc5c5a7214d8be7d707))
* the top row has no Search button ([#124](https://github.com/nerdterm/nerd/issues/124)) ([5cedf38](https://github.com/nerdterm/nerd/commit/5cedf38c5c00a690a16cfe0d6114e476a1738f51))

## [0.17.0](https://github.com/nerdterm/nerd/compare/v0.16.0...v0.17.0) (2026-10-23)


### Added

* one Search window finds panes, commands, settings, plugins, files, help and pane text ([#113](https://github.com/nerdterm/nerd/issues/113)) ([e5894f4](https://github.com/nerdterm/nerd/commit/e5894f4d41b11e74898f12e31676e207fe9cf268))

## [0.16.0](https://github.com/nerdterm/nerd/compare/v0.15.0...v0.16.0) (2026-10-22)


### Added

* Servers… lists every server, a bare nerd reopens the last one, and a server nothing was done in is not kept ([#110](https://github.com/nerdterm/nerd/issues/110)) ([d25802b](https://github.com/nerdterm/nerd/commit/d25802b179ec20dfd07602d8b3dcfecdad8a7d95))


### Fixed

* the release script names nerdterm/nerd for gh, and the docs say nerd-plugins checks the signature ([#107](https://github.com/nerdterm/nerd/issues/107)) ([545dbb5](https://github.com/nerdterm/nerd/commit/545dbb56ef48d10e5bd0523fc172ea6de42ba406))

## [0.15.0](https://github.com/nerdterm/nerd/compare/v0.14.0...v0.15.0) (2026-10-22)


### Added

* releases are signed, and nerd installs only what its release key signed ([#105](https://github.com/nerdterm/nerd/issues/105)) ([1cb52c8](https://github.com/nerdterm/nerd/commit/1cb52c88a9f3fe8fbdc73d334ed78781afe79c0c))

## [0.14.0](https://github.com/nerdterm/nerd/compare/v0.13.0...v0.14.0) (2026-10-21)


### Added

* a held message opens on itself with its reason and its buttons ([#93](https://github.com/nerdterm/nerd/issues/93)) ([a5159e2](https://github.com/nerdterm/nerd/commit/a5159e2347567f30b684f9063bbeb095239301ff))
* a quick start for messages in the Messages window, the docs and the help ([#104](https://github.com/nerdterm/nerd/issues/104)) ([789dffd](https://github.com/nerdterm/nerd/commit/789dffdac393be3b43095eb2c276a2d998c3f313))
* brew install nerdterm/tap/nerd ([#89](https://github.com/nerdterm/nerd/issues/89)) ([1f76248](https://github.com/nerdterm/nerd/commit/1f762484b4bc0466e34414d1bd7bc6fc9e15f196))
* check for a new release from the menu, and install it on a click ([#92](https://github.com/nerdterm/nerd/issues/92)) ([77c3c86](https://github.com/nerdterm/nerd/commit/77c3c86d7c20a3888a7e39ac196e0f9965a0562e))


### Fixed

* a lone pane is named from its space's row, and nerd agents names the caller ([#96](https://github.com/nerdterm/nerd/issues/96)) ([c76e9ee](https://github.com/nerdterm/nerd/commit/c76e9ee4ceeee0d107c6ce8bc877a3b00531322a))
* terminal preset follow-ups from the [#79](https://github.com/nerdterm/nerd/issues/79) review ([#87](https://github.com/nerdterm/nerd/issues/87)) ([e087a40](https://github.com/nerdterm/nerd/commit/e087a401934a533ed1762b7f19757383f14d8edf))

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
