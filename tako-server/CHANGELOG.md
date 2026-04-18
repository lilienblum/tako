# Changelog

## [0.4.0](https://github.com/lilienblum/tako/compare/tako-server-v0.3.0...tako-server-v0.4.0) (2026-04-18)


### Features

* **channels:** add durable pub-sub channels across SDKs and server ([dd1ef52](https://github.com/lilienblum/tako/commit/dd1ef52ff0ef623a3d91d4474033c0b3ae700cd4))
* **channels:** add tako-channels crate and SDK channels module ([0280153](https://github.com/lilienblum/tako/commit/02801535f6b19c200cfadaa98998262bcef3c35b))
* **runtime:** add app data dirs and graceful restart ([acd1fdd](https://github.com/lilienblum/tako/commit/acd1fdd143e991cb683a3a30292cb613e67fa4b2))
* **runtime:** signal app readiness over fd 4 instead of stdout ([7ee5f8a](https://github.com/lilienblum/tako/commit/7ee5f8accfe8e886e322d88df2a7dd61bf62bdf0))
* **website:** add .local domains and durable workflows cards, regroup feature list ([803f640](https://github.com/lilienblum/tako/commit/803f640e253889c01c60ff40de6a1dd9acc1c724))
* **workflows:** durable workflow engine with runs, steps, signals ([8185013](https://github.com/lilienblum/tako/commit/8185013ba1d92a10905dc0fd1cbb7ad8a8a2004b))


### Bug Fixes

* **security:** upgrade nanoid to 0.5 to drop rand 0.8 (GHSA-cq8v-f236-94qc) ([ce1a875](https://github.com/lilienblum/tako/commit/ce1a875c726ae3c79d05dab7708c152396579c5c))
* **server:** set FD_CLOEXEC on bootstrap pipe write end ([91aaed0](https://github.com/lilienblum/tako/commit/91aaed0eb591c31040570491f888afac090f2626))
* **workflows:** worker_id guard + secrets via fd 3 ([1675fdd](https://github.com/lilienblum/tako/commit/1675fdd0e22501215db27da653f85c5065d8777c))

## [0.3.0](https://github.com/lilienblum/tako/compare/tako-server-v0.2.0...tako-server-v0.3.0) (2026-04-10)

### Features

- **dev:** add LAN mode for real-device testing ([c955027](https://github.com/lilienblum/tako/commit/c9550274edd4efe1831192776c584ae442235f02))

## [0.2.0](https://github.com/lilienblum/tako/compare/tako-server-v0.1.0...tako-server-v0.2.0) (2026-04-07)

### Features

- **nextjs:** add Next.js preset, SDK adapter, and raise cold start queue ([9d40f06](https://github.com/lilienblum/tako/commit/9d40f06f83bfd47dc5fd2bf7437a339a775fe688))

## [0.1.0](https://github.com/lilienblum/tako/compare/tako-server-v0.0.1...tako-server-v0.1.0) (2026-04-03)

### Features

- **cli:** rename secrets file to secrets.json, track in git via init ([cf8dc5a](https://github.com/lilienblum/tako/commit/cf8dc5a5dc1477a5705e951690e7aad6ff035ce7))
- **deploy:** add PrepareRelease step, improve build stages and deploy UX ([8891b9b](https://github.com/lilienblum/tako/commit/8891b9bc19aa1c0899ebadf39bf9762d1cae757f))
- **deploy:** support monorepo workspace deploys ([8fc2b06](https://github.com/lilienblum/tako/commit/8fc2b06ad1908d59780bf2e219e690e5e7dd914d))
- **dev:** add path-based route matching, dynamic TLS, and PID locking ([bcdd3ea](https://github.com/lilienblum/tako/commit/bcdd3eaebd3141d4c60985a47f3b3d315d415634))
- **dev:** add process exit monitoring and startup readiness check ([e8797f8](https://github.com/lilienblum/tako/commit/e8797f81339ef59259d437a3280dd759b384d760))
- **dev:** overhaul dev architecture — streaming output, persistent state, `.tako` domain ([1d63925](https://github.com/lilienblum/tako/commit/1d63925db745eb60f96dd5d40dcdd9684393870a))
- migrate dev domain to .tako.test, parallel upgrades, deploy preflight, and SSH optimizations ([74e0391](https://github.com/lilienblum/tako/commit/74e03914f4367aa0d09e1c81848785811204ef0c))
- **proxy:** set downstream keepalive request limits ([d80bdc4](https://github.com/lilienblum/tako/commit/d80bdc4e02182ee4f059517282999dd5dfe36023))
- replace Docker builds with cargo-zigbuild and vendored OpenSSL ([bf91fd5](https://github.com/lilienblum/tako/commit/bf91fd57e27c828f59a95524b9860c0d8e06e65e))
- **repo:** restructure CLI output, add task tree UI, simplify server networking ([ab57a2e](https://github.com/lilienblum/tako/commit/ab57a2eb484b1ecca65f2f5fd6af4f188243379c))
- **scale:** move instance scaling to runtime state ([f105611](https://github.com/lilienblum/tako/commit/f1056118c318c3e1c35bbf70417ea9c990bfd321))
- **secrets:** deliver secrets via temp file and add SDK Secrets API ([02f707e](https://github.com/lilienblum/tako/commit/02f707e8fa27b21d43a411da14fc76b3a8b59bd2))
- **secrets:** make secrets per-app with hash-based deploy sync ([68aae96](https://github.com/lilienblum/tako/commit/68aae96d3f03ee114304d33462af0e9f2a977527))
- **secrets:** pass secrets via fd 3 instead of POST /secrets endpoint ([22b02f7](https://github.com/lilienblum/tako/commit/22b02f7408f2cb556b7c5116a6e7411a3cf9ae24))
- **secrets:** store secrets encrypted in SQLite, push to instances via socket ([0efa3ad](https://github.com/lilienblum/tako/commit/0efa3adae5999abb1f2d0ef76c9ea391ff804fc8))
- **server:** add `tako servers setup-wildcard`, consolidate lego to server ([c083134](https://github.com/lilienblum/tako/commit/c0831343c9e69969bd79f86962430163b209ae34))
- **server:** add mimalloc allocator, Prometheus metrics, and server name ([14ef5b0](https://github.com/lilienblum/tako/commit/14ef5b02c4f67832ca06c46f0573776e6bcd4c62))
- **server:** add per-app log files with rotation and backpressure ([1f40951](https://github.com/lilienblum/tako/commit/1f409514e1a56ba32518743ce2ffad92c9afb6ec))
- **server:** add per-IP concurrent request limiting for DDoS mitigation ([8445381](https://github.com/lilienblum/tako/commit/8445381a8c98dd2bac8b69c9c14bbf4f1426edf2))
- **server:** add server-side production install after artifact extraction ([467ed89](https://github.com/lilienblum/tako/commit/467ed898e4876c5b9217efd0c94edadd101cbe76))
- **server:** auto-install lego for wildcard DNS-01 certificates ([02b2bac](https://github.com/lilienblum/tako/commit/02b2bacf5957cbb07a0bc694a1689ec5f64306bb))
- **server:** multi-environment deployment identity ({app}/{env}) ([dd2863b](https://github.com/lilienblum/tako/commit/dd2863b4b90ab56ea82b6cf798a77405e8bc8cd6))
- **server:** replace proto with direct binary download engine ([b892e5c](https://github.com/lilienblum/tako/commit/b892e5c3dc9252ce707151ad8338dc7ff0e19e83))
- **server:** switch app upstreams to private tcp ([1996d13](https://github.com/lilienblum/tako/commit/1996d13edcf2826fea9a7d4c1732b53b0e611b58))
- **tako-server:** add --worker mode for hot standby ([c31c294](https://github.com/lilienblum/tako/commit/c31c2946a5055140b2f47b11460edb660519be24))
- **tls:** add wildcard TLS certificates via lego DNS-01 ([5a92357](https://github.com/lilienblum/tako/commit/5a92357e8df007c53711e4a21bcb8acc8073edd9))

### Bug Fixes

- **acme:** write HTTP-01 challenge response directly in request_filter ([ceabba1](https://github.com/lilienblum/tako/commit/ceabba14e997c79027da7e58b6335434aee535e2))
- **ci:** update test fixtures for dist entrypoints and add Go SDK readiness signal ([1bac303](https://github.com/lilienblum/tako/commit/1bac3033d6397c203c6ed5518807718ecb8b486b))
- **deploy:** copy from project dir not git root, remove app_subdir ([065f510](https://github.com/lilienblum/tako/commit/065f510c1cd904d71ac6a359512598da44593b1d))
- **deploy:** warn user when runtime version cannot be detected ([cfeb47a](https://github.com/lilienblum/tako/commit/cfeb47aa5354528a5b4524a2ea0b437bb4618855))
- **e2e:** add verbose deploy, server log dump, and proto detection logging ([f664f34](https://github.com/lilienblum/tako/commit/f664f3471b1f467e46cb3155774bad3552196b18))
- **e2e:** generate sha256 file for dummy server archive ([0253ea6](https://github.com/lilienblum/tako/commit/0253ea6f94a784c7b127e4db566493bce28d2739))
- log ACME challenge error details on order failure ([4400b68](https://github.com/lilienblum/tako/commit/4400b6881e8ff38f63730313dc7132815d00641e))
- **proxy:** use URI authority for host extraction to support HTTP/2 ([e3605fe](https://github.com/lilienblum/tako/commit/e3605fede9b40ca3cc7b448deaccfb1d629fecc4))
- remove await on sync handleTakoEndpoint and suppress clippy warning ([8f349f6](https://github.com/lilienblum/tako/commit/8f349f64133548b0d7049c523ca477c7a8f1fcd3))
- restrict socket/secrets permissions, fix secrets hash collision ([8f01270](https://github.com/lilienblum/tako/commit/8f01270348676a4c31c3de63d514dda99f76fb2c))
- **secrets:** prevent fd 3 read from blocking when not a Tako pipe ([241c671](https://github.com/lilienblum/tako/commit/241c67146680998ce3dd3b44cddaa8be1721adf5))
- **security:** delete secrets when deleting an app ([178dbbb](https://github.com/lilienblum/tako/commit/178dbbb872831353b7679f4711081bbd598b658f))
- **security:** drop root privileges for production install, quote shell paths ([f50e118](https://github.com/lilienblum/tako/commit/f50e1186dfaa8d15b4dbbd67ebc3bb6069411e48))
- **security:** harden shell quoting and strip internal token header ([4f5af85](https://github.com/lilienblum/tako/commit/4f5af858df205a0de04f3f190c495f6528e124fe))
- **security:** resolve code scanning alerts ([a55edc6](https://github.com/lilienblum/tako/commit/a55edc64206cd6697fcc73b7245c350c3ebd8a5f))
- **security:** validate runtime version string against path traversal ([0bdbe26](https://github.com/lilienblum/tako/commit/0bdbe2678621e3b44ae9b590c8fa160e5e732203))
- **server:** add missing idle_timeout to test app.json fixtures ([1a8dd5b](https://github.com/lilienblum/tako/commit/1a8dd5b07093cd84d95150c26ab559bda6f58c4c))
- **server:** add request body size limit to prevent memory exhaustion ([f67ef1b](https://github.com/lilienblum/tako/commit/f67ef1b74a12a9900f658b8654246c30f3b4624f))
- **server:** add upstream peer timeouts to prevent indefinite request hangs ([fd25b90](https://github.com/lilienblum/tako/commit/fd25b90e2d2b72e052f3f801b81b37911b84e5c7))
- **server:** cap health check response buffer at 4 KB ([979f025](https://github.com/lilienblum/tako/commit/979f02508b262b873bdcf1c4db855be9f4e57e7e))
- **server:** case-insensitive hostname matching per RFC 7230 ([847a42d](https://github.com/lilienblum/tako/commit/847a42da2d3f5a9c75b5fb966c0444598025fe31))
- **server:** clear only per-domain ACME challenge tokens, not all ([faa11ed](https://github.com/lilienblum/tako/commit/faa11ededdd8e579ec7d8c116eb2c5bb7c717898))
- **server:** create secrets file with 0600 from the start ([973efe4](https://github.com/lilienblum/tako/commit/973efe47c0d9e94d196d38b007b93175463c6317))
- **server:** detect new process by PID during zero-downtime upgrade ([50b9a62](https://github.com/lilienblum/tako/commit/50b9a62e79116a9cd2d360a5124248eec5477219))
- **server:** download package manager when it differs from runtime ([41eedb4](https://github.com/lilienblum/tako/commit/41eedb446045910e5736181d644a8194a3a419bf))
- **server:** drain app stdout/stderr pipes to prevent process stalls ([8a4a036](https://github.com/lilienblum/tako/commit/8a4a0362bc698c535ac3caf9d70a63611a152c19))
- **server:** drop privileges for all deploy-time commands, not just install ([c81b21a](https://github.com/lilienblum/tako/commit/c81b21ae6f1cd716092ce0c7a8dfc2afbf89f201))
- **server:** drop privileges to tako-app for install commands ([044587f](https://github.com/lilienblum/tako/commit/044587f6f8b67da13f835fecee0edde702ce2e8e))
- **server:** fix log rotation file handle, tail -F, and timestamp sorting ([740cc5d](https://github.com/lilienblum/tako/commit/740cc5dd5ade2ab50dabed8e3c5da0489e9b7825))
- **server:** fix static file path traversal, stream EOF, IP tracker underflow ([1cd3d6a](https://github.com/lilienblum/tako/commit/1cd3d6aa4a8346fc14b88f9bd3477ba6bbbe6829))
- **server:** pre-install mise tools to prevent cold start timeouts ([cda048b](https://github.com/lilienblum/tako/commit/cda048bb33786f5620a1ea2e40d58c39c6928d63))
- **server:** preserve query string in HTTP→HTTPS redirect ([1c959df](https://github.com/lilienblum/tako/commit/1c959dfdff7977adec632ec41d16f29ffd0f75f2))
- **server:** remove oversized sqlite cache_size and mmap_size pragmas ([7de7440](https://github.com/lilienblum/tako/commit/7de74408361a29a993bb40d053e68f685abe2c3a))
- **server:** resolve exe path at startup for SIGHUP reload, trust mise in instance env ([6e11c02](https://github.com/lilienblum/tako/commit/6e11c02c8885c735b4f00df86129970e290af648))
- **server:** security hardening and dead code removal ([13fbd47](https://github.com/lilienblum/tako/commit/13fbd479659ed0eb5ccf706bb0da915175c6f0c6))
- **server:** set authoritative forwarded headers, strip client-supplied values ([f6b0c35](https://github.com/lilienblum/tako/commit/f6b0c3596e8cf9526708f4dadbd35db1032da082))
- **server:** stabilize flaky on_demand integration test ([c157b44](https://github.com/lilienblum/tako/commit/c157b44b2a0b76bc23461b0f4b17eeb65eef1782))
- **server:** use MISE_TRUSTED_CONFIG_PATHS to trust release configs ([f11a21f](https://github.com/lilienblum/tako/commit/f11a21fa12e7a945ce813117c5703f7f0e2735fd))
- SSH exit code handling, upgrade reliability, and wizard UX improvements ([4e761a4](https://github.com/lilienblum/tako/commit/4e761a4020c21bffe76a0d69c716401a86d42406))
- **sweep:** perf, security, quality fixes across server, runtime, SDK ([938959b](https://github.com/lilienblum/tako/commit/938959b8dd0c3c0879867c47cd32e3d665113c72))
- **tako-server:** ensure runtime binaries are on PATH for install commands ([fd057a0](https://github.com/lilienblum/tako/commit/fd057a04d9705860384ef2a7f634a0a1bd97c989))
- **tako-server:** set MISE_TRUST_ALL=1 when running release install commands ([9399997](https://github.com/lilienblum/tako/commit/9399997625ddf1d10ce1de7381f0521cc694c1c1))
- **tako-server:** use "latest" runtime version when none specified ([ef9fb1b](https://github.com/lilienblum/tako/commit/ef9fb1b7485e56e2e82afe767aad0a33659e7c25))
- **tako-server:** use resolved runtime binary path for dependency install ([0285493](https://github.com/lilienblum/tako/commit/0285493012e005cf0887522368871a3fc10583b2))
- **test:** fix deploy_on_demand warm instance test ([4f42d2e](https://github.com/lilienblum/tako/commit/4f42d2ea9cdbe66c486762f988fff4cd8d18b17b))
- **tls:** serve full certificate chain and auto-reload after ACME renewal ([441a6f0](https://github.com/lilienblum/tako/commit/441a6f00a70fbf2c3ff45f155f17e687bcb1d5d4))
- **upgrade:** bind management socket before ACME/SQLite init to prevent timeout ([8b73948](https://github.com/lilienblum/tako/commit/8b73948ebbf86f4ef53ae08280d2af6d0f5aedce))
- use correct mise env var, skip redundant deploy prompt, add trailing newline ([31aa726](https://github.com/lilienblum/tako/commit/31aa726680c34a77f35b3cd002e9cc70068879bc))

### Performance

- **lb:** eliminate double DashMap scan in round_robin and ip_hash ([287f873](https://github.com/lilienblum/tako/commit/287f87359a65f81df5063b0383ffea5a406f546a))
- **server:** cache parsed TLS certs in memory to eliminate per-handshake disk reads ([73e8ed3](https://github.com/lilienblum/tako/commit/73e8ed39fdd0b1ac8f8dfc5e7476c39cbdff0108))
- **server:** eliminate per-request String clone in RequestTimer ([2eca1ba](https://github.com/lilienblum/tako/commit/2eca1baa96e1f4a6d8923e2e661f7624159137cc))
- **server:** rate-limit per-handshake TLS warnings to prevent log flooding ([b0d981b](https://github.com/lilienblum/tako/commit/b0d981b26c1aa4a6e0d118e8c431661a025ad3c7))
- **server:** use non-blocking tracing writer to prevent Tokio stalls ([4d3a922](https://github.com/lilienblum/tako/commit/4d3a922ce7f2794996c19d37b7a85afb77aa7227))
