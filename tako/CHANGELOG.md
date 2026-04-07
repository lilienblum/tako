# Changelog

## [0.2.0](https://github.com/lilienblum/tako/compare/tako-v0.1.0...tako-v0.2.0) (2026-04-07)


### Features

* **nextjs:** add Next.js preset, SDK adapter, and raise cold start queue ([9d40f06](https://github.com/lilienblum/tako/commit/9d40f06f83bfd47dc5fd2bf7437a339a775fe688))


### Bug Fixes

* **ci:** authenticate GitHub API calls for preset fetching ([3047ddc](https://github.com/lilienblum/tako/commit/3047ddc3bfdf77e2ff13a94f6d14e0559b27d91c))
* **deploy:** clean up lock file on drop to prevent stale flock contention ([6ce0f57](https://github.com/lilienblum/tako/commit/6ce0f57cfd4efa4e0c1e5fda7b39ae36fb7918b6))
* **deploy:** lock local deploy execution ([a14626d](https://github.com/lilienblum/tako/commit/a14626dbc978feb4e0ce302202aa5ac16e0c669c))
* **deploy:** prune stale local build caches ([63a87c8](https://github.com/lilienblum/tako/commit/63a87c81656e5acbd14866023e489d465ecb710f))
* **dev:** kill orphaned app processes from previous crashed runs ([7a3aa81](https://github.com/lilienblum/tako/commit/7a3aa8182b994781c2ce097cf8c770cf8fc947d3))
* **dev:** prepend node_modules/.bin to PATH when spawning app process ([7791420](https://github.com/lilienblum/tako/commit/7791420e5506256d3a6da15bacbb86222e74f175))
* **repo:** update preset resolution and deploy cleanup ([628f6c4](https://github.com/lilienblum/tako/commit/628f6c42abf6512c31c26a166c47cfeff838d243))

## 0.1.0 (2026-04-03)

### Features

- **build:** add workdir module for clean deploy builds ([0e01aeb](https://github.com/lilienblum/tako/commit/0e01aebd7b33f0c333f8306795462421a13d12da))
- **build:** simplify presets to metadata-only, remove container builds ([a7a403d](https://github.com/lilienblum/tako/commit/a7a403d80f0d6e009acc40787250abae61b9d2db))
- **cli:** accept Go runtime in deploy and dev commands ([5623f25](https://github.com/lilienblum/tako/commit/5623f2570396dc35ea99f319b15ad8b10070dbe5))
- **cli:** add `tako implode` and `tako servers implode` commands ([7714fd8](https://github.com/lilienblum/tako/commit/7714fd8633a16d65b857434632ecd301ce42ab3f))
- **cli:** add Go build adapter and preset group ([bb5491b](https://github.com/lilienblum/tako/commit/bb5491b5811637d279df4199f453fef734d4cad7))
- **cli:** add tako typegen command for Go and JS ([cec06e6](https://github.com/lilienblum/tako/commit/cec06e6661cb97c441586f4f3b00308369bddf1e))
- **cli:** prompt for alternative config name when declining overwrite in tako init ([150db0d](https://github.com/lilienblum/tako/commit/150db0d879e231c466c32eac0a2e2cb24efe9e1f))
- **config:** restructure tako.toml schema for workdir deploy ([a61605e](https://github.com/lilienblum/tako/commit/a61605ebad6c93feb95f7463e89034bab49d6f0b))
- **deploy:** add PrepareRelease step, improve build stages and deploy UX ([8891b9b](https://github.com/lilienblum/tako/commit/8891b9bc19aa1c0899ebadf39bf9762d1cae757f))
- **deploy:** reuse preflight SSH connections, fix output ordering, polish UX ([85ae539](https://github.com/lilienblum/tako/commit/85ae5396d3a8c15315dea8981a2af4ca8b906796))
- **deploy:** rewrite deploy flow with workdir and build.run/install ([ab4c172](https://github.com/lilienblum/tako/commit/ab4c1729f33a42890df59958e5500d61384900ec))
- **deploy:** support monorepo workspace deploys ([8fc2b06](https://github.com/lilienblum/tako/commit/8fc2b06ad1908d59780bf2e219e690e5e7dd914d))
- **dev:** add Linux portless dev mode via iptables redirect ([f79768d](https://github.com/lilienblum/tako/commit/f79768d3aa44ed6ffbd4bbae6817cedf02f22b31))
- **dev:** add process exit monitoring and startup readiness check ([e8797f8](https://github.com/lilienblum/tako/commit/e8797f81339ef59259d437a3280dd759b384d760))
- **repo:** restructure CLI output, add task tree UI, simplify server networking ([ab57a2e](https://github.com/lilienblum/tako/commit/ab57a2eb484b1ecca65f2f5fd6af4f188243379c))
- **server:** add `tako servers setup-wildcard`, consolidate lego to server ([c083134](https://github.com/lilienblum/tako/commit/c0831343c9e69969bd79f86962430163b209ae34))
- **server:** add per-app log files with rotation and backpressure ([1f40951](https://github.com/lilienblum/tako/commit/1f409514e1a56ba32518743ce2ffad92c9afb6ec))
- **server:** switch app upstreams to private tcp ([1996d13](https://github.com/lilienblum/tako/commit/1996d13edcf2826fea9a7d4c1732b53b0e611b58))

### Bug Fixes

- **ci:** add environment to fix-lockfile job; skip SDK install in non-interactive init ([cbda3a2](https://github.com/lilienblum/tako/commit/cbda3a23fb408f386cb3661b436a471cb5de75bd))
- **ci:** allow non-interactive CA trust install when running as root ([68f20fe](https://github.com/lilienblum/tako/commit/68f20fe114b75a7b9e69b5803e364bc08783ca1d))
- **ci:** restore version field for release-please managed crates ([c24001e](https://github.com/lilienblum/tako/commit/c24001e015bdbd68af0f72611f6315281bac0fd6))
- **ci:** update test fixtures for dist entrypoints and add Go SDK readiness signal ([1bac303](https://github.com/lilienblum/tako/commit/1bac3033d6397c203c6ed5518807718ecb8b486b))
- **cli:** handle duplicate CA certificates in macOS keychain ([b0aa13a](https://github.com/lilienblum/tako/commit/b0aa13a7107a696b0ca09cd74c1b584672d60f84))
- **cli:** handle missing base runtime presets and optional preset step in tests ([589c977](https://github.com/lilienblum/tako/commit/589c97732998d8a4a484f4cf8c8c740009f7cbf8))
- **cli:** show full canary version after upgrade ([44f9f17](https://github.com/lilienblum/tako/commit/44f9f179d85bff0ba74a56ddde48024c00d66a92))
- **deploy:** copy from project dir not git root, remove app_subdir ([065f510](https://github.com/lilienblum/tako/commit/065f510c1cd904d71ac6a359512598da44593b1d))
- **deploy:** default build CWD to app directory within workspace ([5ee6140](https://github.com/lilienblum/tako/commit/5ee61403a96931da7ae26d5e279cf7341e9e6287))
- **deploy:** resolve asset directories relative to app dir, not workspace root ([49f44f2](https://github.com/lilienblum/tako/commit/49f44f22b99e723a303aa4984e721df20ce2a498))
- **dev:** skip all interactive gates when running as root ([9f9fcbb](https://github.com/lilienblum/tako/commit/9f9fcbb6547c224a9bfaeddca45ede6a6808fcdf))
- **init:** use global \*_/.tako/_ gitignore rules instead of path-specific ones ([8d8da20](https://github.com/lilienblum/tako/commit/8d8da200c7a64e63fc6292de8f82e912cc694edf))
- **lint:** resolve all clippy and oxlint warnings ([37eb2cf](https://github.com/lilienblum/tako/commit/37eb2cf2c7a3c3789ba669100a840c011eb09028))
- **repo:** verify signed server upgrade artifacts ([d4d2625](https://github.com/lilienblum/tako/commit/d4d2625beefc875d795f7ae5a947786d23b38d77))
- **security:** drop root privileges for production install, quote shell paths ([f50e118](https://github.com/lilienblum/tako/commit/f50e1186dfaa8d15b4dbbd67ebc3bb6069411e48))
- **security:** harden shell quoting and strip internal token header ([4f5af85](https://github.com/lilienblum/tako/commit/4f5af858df205a0de04f3f190c495f6528e124fe))
- **security:** resolve code scanning alerts ([9876eb9](https://github.com/lilienblum/tako/commit/9876eb92b5a7c5c90d507908ac5f57fbc09561fe))
- **security:** resolve Dependabot alerts by upgrading dependencies ([f7b429a](https://github.com/lilienblum/tako/commit/f7b429a14198fb5d31ffd0bd97165776c229719e))
- **server:** fix log rotation file handle, tail -F, and timestamp sorting ([740cc5d](https://github.com/lilienblum/tako/commit/740cc5dd5ade2ab50dabed8e3c5da0489e9b7825))
- **server:** remove oversized sqlite cache_size and mmap_size pragmas ([7de7440](https://github.com/lilienblum/tako/commit/7de74408361a29a993bb40d053e68f685abe2c3a))
- **server:** use password input for DNS credential prompts ([0c0a7b4](https://github.com/lilienblum/tako/commit/0c0a7b4db2aa2be4158c0a8306dd22e4f0f3e67c))
- **ssh:** report dropped connections as failures instead of success ([0f808d9](https://github.com/lilienblum/tako/commit/0f808d9cbb569411f0a11cbc10eafa3e8129e451))
- **sweep:** perf, security, quality fixes across server, runtime, SDK ([938959b](https://github.com/lilienblum/tako/commit/938959b8dd0c3c0879867c47cd32e3d665113c72))
- **tako:** run server upgrades through sudo shell ([c227986](https://github.com/lilienblum/tako/commit/c227986d4b1c8eafae8c2434af944a206f609f5c))
- **tako:** skip release signature verification for custom download sources ([e0a5485](https://github.com/lilienblum/tako/commit/e0a54854d28ba7f505a7bbaeedcf8e7ad718f936))
