---
layout: ../../layouts/BlogPostLayout.astro
title: "From Zero to Production: One Server, One Command"
date: "2026-04-05T14:04"
description: "You just bought a VPS. Here's the end-to-end walkthrough to get your app live with real HTTPS, zero downtime, and no YAML."
image: 33baa22b4174
---

You just bought a VPS. Hetzner, DigitalOcean, OVH, whatever — the welcome email is still warm, and you have an IP address and an SSH key. Now you want to put an app on it: real domain, real HTTPS, rolling updates, the works.

Here's the whole path, start to finish. No Docker, no YAML, no reverse proxy config files.

## The five commands

```d2
direction: right

vps: Fresh VPS {
  shape: cylinder
}

server: tako-server {
  shape: hexagon
}

local: Your laptop {
  shape: rectangle
}

app: Your app live {
  shape: cloud
}

vps -> server: "1. install-server.sh"
local -> server: "2. servers add"
local -> server: "3. init + deploy"
server -> app: "4. Pingora routes traffic"
```

That's the whole mental model. Two machines, four steps, one app. Let's walk through it.

## Step 1 — Install the server runtime

SSH into your VPS as root and run one line:

```bash
sudo sh -c "$(curl -fsSL https://tako.sh/install-server.sh)"
```

The installer detects your architecture and libc (glibc or musl), downloads the matching `tako-server` binary, creates the `tako` and `tako-app` system users, and wires up a systemd or OpenRC service. When it finishes, `tako-server` is running and listening on ports 80 and 443 via `CAP_NET_BIND_SERVICE` — no reverse proxy to install, no Nginx to configure.

That's the entire server setup. You never have to SSH back in again.

## Step 2 — Install the CLI and register the server

Back on your laptop:

```bash
curl -fsSL https://tako.sh/install.sh | sh
tako servers add 1.2.3.4 --name prod
```

[`tako servers add`](/docs/cli) tests the SSH connection, detects the target's build metadata, and records the server in your global [`config.toml`](/docs/tako-toml). It uses your existing `~/.ssh` keys — nothing new to generate.

## Step 3 — Configure your app

From your project directory:

```bash
tako init
```

Interactive prompts ask for an app name and your production route (e.g. `myapp.example.com`), then write a minimal [`tako.toml`](/docs/tako-toml). The same command installs the [`tako.sh` SDK](/docs) into your package manager, pins your runtime version, and updates `.gitignore` so encrypted secrets can stay in the repo safely.

Point your DNS `A` record at the server's IP. That's the only manual piece — everything else is automated.

## Step 4 — Deploy

```bash
tako deploy
```

Tako builds your app locally, bundles the artifact, and ships it to the server over SFTP. On the other end, `tako-server` extracts the archive, runs the production install (`bun install --production`, `npm ci`, or the equivalent for your runtime), starts one warm instance, and waits for the SDK's readiness signal before flipping traffic over. The first deploy also kicks off an ACME challenge, so your route is serving real Let's Encrypt HTTPS within seconds of the process coming up.

No registry. No image builds. No certificate wrangling. A one-line code change redeploys in [seconds, not minutes](/blog/why-we-dont-default-to-docker).

## What you didn't have to do

Count the things that didn't happen: you didn't write a Dockerfile, install Nginx, provision certificates, set up a container registry, or copy a `docker-compose.yml` between environments. You didn't edit `/etc/systemd/system`. You didn't configure a firewall rule for the proxy because the proxy is the server.

And because the same [Pingora-based proxy](/blog/pingora-vs-caddy-vs-traefik) runs locally under [`tako dev`](/blog/local-dev-with-real-https), the app you tested at `https://myapp.tako.test/` behaves identically on your VPS. No "works on my machine."

When you're ready for a second app on the same box, just repeat step 3 in another project. `tako-server` handles multiple apps, [scales idle ones to zero](/blog/scale-to-zero-without-containers), and rolls updates one instance at a time. When you're ready for a second VPS, [add it to an environment](/docs/deployment) and `tako deploy` hits them both.

That's zero to production. One server, one command, one tool. Read [how Tako works](/docs/how-tako-works) for the full architecture, or jump into the [quickstart](/docs/quickstart) to try it yourself.
