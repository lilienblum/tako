# Changelog

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
