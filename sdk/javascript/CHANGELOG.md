# Changelog

## [0.7.0](https://github.com/lilienblum/tako/compare/sdk-js-v0.6.0...sdk-js-v0.7.0) (2026-04-19)


### Features

* **workflows:** run dev worker as scale-to-zero subprocess with crash-loop detection ([07a397f](https://github.com/lilienblum/tako/commit/07a397f14c1a30061a205a76f3ce52f362ce3845))


### Bug Fixes

* **sdk-js:** resolve code scanning alerts ([a6e4066](https://github.com/lilienblum/tako/commit/a6e4066c3d6685d9607c187760228f4b9e6b4f50))

## [0.6.0](https://github.com/lilienblum/tako/compare/sdk-js-v0.5.0...sdk-js-v0.6.0) (2026-04-18)


### Features

* **channels:** add tako-channels crate and SDK channels module ([0280153](https://github.com/lilienblum/tako/commit/02801535f6b19c200cfadaa98998262bcef3c35b))

## [0.5.0](https://github.com/lilienblum/tako/compare/sdk-js-v0.4.0...sdk-js-v0.5.0) (2026-04-16)


### Features

* ambient Tako global, typegen rewrite, LOG_LEVEL, file-based CA ([a39773b](https://github.com/lilienblum/tako/commit/a39773b88cc65fda553bee9dfc378e158674a5b5))
* **channels:** add durable pub-sub channels across SDKs and server ([dd1ef52](https://github.com/lilienblum/tako/commit/dd1ef52ff0ef623a3d91d4474033c0b3ae700cd4))
* **runtime:** signal app readiness over fd 4 instead of stdout ([7ee5f8a](https://github.com/lilienblum/tako/commit/7ee5f8accfe8e886e322d88df2a7dd61bf62bdf0))
* **sdk:** defineWorkflow returns WorkflowDefinition object ([184a5c5](https://github.com/lilienblum/tako/commit/184a5c56b78d534c760ebb1b45d799c15956a3b0))
* **sdk:** export defineWorkflow, isWorkflowDefinition, WorkflowDefinition ([b1a5884](https://github.com/lilienblum/tako/commit/b1a58841a60661d464b0fa304111a204796c077f))
* **sdk:** flatten step namespace into WorkflowContext ([326d7ba](https://github.com/lilienblum/tako/commit/326d7baeb064a8f1fb78bfa0cb3515c7b938aa59))
* **sdk:** flip WorkflowHandler signature to (payload, ctx) ([2994b77](https://github.com/lilienblum/tako/commit/2994b770b3af5ca8f54ead590b7496fa6451b3aa))
* **sdk:** rename maxAttempts -&gt; retries in user-facing workflow API ([a1c2981](https://github.com/lilienblum/tako/commit/a1c29812cb7e2cd0de9f472c489c1b0fed2432d5))
* **sdk:** update workflow discovery to use WorkflowDefinition symbol ([28b7722](https://github.com/lilienblum/tako/commit/28b7722bfe20373ebd0781c9d34f032f26e2afe8))
* **sdk:** WorkflowHandler returns void + WorkflowRegistry typed enqueue + typegen ([9e1d178](https://github.com/lilienblum/tako/commit/9e1d1782380f941addd4bb67116dc0ad3f2185f7))
* **workflows:** durable workflow engine with runs, steps, signals ([8185013](https://github.com/lilienblum/tako/commit/8185013ba1d92a10905dc0fd1cbb7ad8a8a2004b))


### Bug Fixes

* **sdk:** cleanup discover tests + fix tsx in docstring ([0577b97](https://github.com/lilienblum/tako/commit/0577b9730c79a753060b666a2645f2ca434b4ca7))
* **sdk:** rename Run.maxAttempts -&gt; retries; unexport WorkflowRetryConfig ([588fd0e](https://github.com/lilienblum/tako/commit/588fd0e0ed7cbb0e0d15523bf46f945c26dade9b))
* **sdk:** strengthen isWorkflowDefinition guard + clean up define tests ([c680837](https://github.com/lilienblum/tako/commit/c680837f9ebd131f1a6dd5bf3bc83c79031d52e5))
* **sdk:** suppress no-redundant-type-constituents lint on open union enqueue signature ([e0d2fb7](https://github.com/lilienblum/tako/commit/e0d2fb7f94317e453209d1b79002690cc2de365c))
* **sdk:** update defineWorkflow JSDoc example for new (payload, ctx) signature ([b7e55ad](https://github.com/lilienblum/tako/commit/b7e55adbf2e384827da9d351d76fa851ebcb6f6e))
* **sdk:** update step.ts and types.ts comments for flat ctx API ([d089b92](https://github.com/lilienblum/tako/commit/d089b92cfbdf2bce9f0a2bbf47805885730a2efb))
* **workflows:** worker_id guard + secrets via fd 3 ([1675fdd](https://github.com/lilienblum/tako/commit/1675fdd0e22501215db27da653f85c5065d8777c))

## [0.4.0](https://github.com/lilienblum/tako/compare/sdk-js-v0.3.0...sdk-js-v0.4.0) (2026-04-10)


### Features

* **dev:** add LAN mode for real-device testing ([c955027](https://github.com/lilienblum/tako/commit/c9550274edd4efe1831192776c584ae442235f02))

## [0.3.0](https://github.com/lilienblum/tako/compare/sdk-js-v0.2.0...sdk-js-v0.3.0) (2026-04-08)


### Features

* **dev:** short .test domains, client connect/disconnect, and log styling ([15dfedf](https://github.com/lilienblum/tako/commit/15dfedfe3644d0b5fce633af162004106ba8c910))

## [0.2.0](https://github.com/lilienblum/tako/compare/sdk-js-v0.1.0...sdk-js-v0.2.0) (2026-04-07)


### Features

* **nextjs:** add Next.js preset, SDK adapter, and raise cold start queue ([9d40f06](https://github.com/lilienblum/tako/commit/9d40f06f83bfd47dc5fd2bf7437a339a775fe688))


### Bug Fixes

* **ci:** trigger release workflows on published, not created ([bec4b7b](https://github.com/lilienblum/tako/commit/bec4b7bcdf473048c9a6ae92f33616a2aa21ef11))
* **dev:** prepend node_modules/.bin to PATH when spawning app process ([7791420](https://github.com/lilienblum/tako/commit/7791420e5506256d3a6da15bacbb86222e74f175))

## [0.1.0](https://github.com/lilienblum/tako/compare/sdk-js-v0.0.1...sdk-js-v0.1.0) (2026-04-03)

### Features

- **deploy:** add PrepareRelease step, improve build stages and deploy UX ([8891b9b](https://github.com/lilienblum/tako/commit/8891b9bc19aa1c0899ebadf39bf9762d1cae757f))
- **dev:** add process exit monitoring and startup readiness check ([e8797f8](https://github.com/lilienblum/tako/commit/e8797f81339ef59259d437a3280dd759b384d760))
- **repo:** restructure CLI output, add task tree UI, simplify server networking ([ab57a2e](https://github.com/lilienblum/tako/commit/ab57a2eb484b1ecca65f2f5fd6af4f188243379c))
- **sdk:** add agent skills and clean up public API surface ([0677fdf](https://github.com/lilienblum/tako/commit/0677fdf29618fe8153fb2f2fea03839afbbb47ab))
- **secrets:** pass secrets via fd 3 instead of POST /secrets endpoint ([22b02f7](https://github.com/lilienblum/tako/commit/22b02f7408f2cb556b7c5116a6e7411a3cf9ae24))
- **server:** switch app upstreams to private tcp ([1996d13](https://github.com/lilienblum/tako/commit/1996d13edcf2826fea9a7d4c1732b53b0e611b58))

### Bug Fixes

- remove await on sync handleTakoEndpoint and suppress clippy warning ([8f349f6](https://github.com/lilienblum/tako/commit/8f349f64133548b0d7049c523ca477c7a8f1fcd3))
- **sdk:** resolve typecheck errors in create-entrypoint arg parsing ([a2bd3c2](https://github.com/lilienblum/tako/commit/a2bd3c2cb9734fe331b110b1b3914ffa7110de27))
- **sdk:** support { fetch } object export in app entrypoint ([76d0e9b](https://github.com/lilienblum/tako/commit/76d0e9b240775f0362ce027c1b66b39857b9a591))
- **secrets:** prevent fd 3 read from blocking when not a Tako pipe ([241c671](https://github.com/lilienblum/tako/commit/241c67146680998ce3dd3b44cddaa8be1721adf5))
- **security:** harden shell quoting and strip internal token header ([4f5af85](https://github.com/lilienblum/tako/commit/4f5af858df205a0de04f3f190c495f6528e124fe))
- **security:** resolve Dependabot alerts by upgrading dependencies ([f7b429a](https://github.com/lilienblum/tako/commit/f7b429a14198fb5d31ffd0bd97165776c229719e))
- **sweep:** perf, security, quality fixes across server, runtime, SDK ([938959b](https://github.com/lilienblum/tako/commit/938959b8dd0c3c0879867c47cd32e3d665113c72))
