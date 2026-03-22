---
name: lifecycle/deploy-to-tako
description: >-
  Step-by-step guide to deploy a Go app with Tako:
  SDK integration, tako.toml configuration, typed secrets,
  build setup, deployment.
type: lifecycle
library: tako.sh
library_version: "0.0.1"
requires:
  - tako-sdk
sources:
  - lilienblum/tako:SPEC.md
---

# Deploy a Go App with Tako

This guide covers adding Tako to a new or existing Go project. Complete the steps in order.

> **CRITICAL**: Your Go app must use the Tako SDK (`tako.sh`). Call `tako.ListenAndServe()` to handle the Tako protocol — there is no separate entrypoint binary like in JavaScript.

> **CRITICAL**: `tako.toml` is your project's deployment config. It lives in the project root alongside `go.mod`.

## Step 1: Install the SDK

```bash
go get tako.sh
```

## Step 2: Create tako.toml

```toml
# tako.toml — minimal config for a Go app
name = "my-app"
runtime = "go"
main = "app"

[build]
run = "CGO_ENABLED=0 go build -o app ."
```

### Key tako.toml fields

| Field         | Purpose                                                        |
| ------------- | -------------------------------------------------------------- |
| `name`        | App name (used in deploy paths)                                |
| `runtime`     | Must be `"go"` for Go apps                                     |
| `main`        | Binary name (output of `go build -o <name>`)                   |
| `[build].run` | Build command (`CGO_ENABLED=0` required for cross-compilation) |
| `[build].cwd` | Working directory for build (for monorepos)                    |
| `assets`      | Static assets directory to serve                               |

### Environment-specific configuration

```toml
name = "my-app"
runtime = "go"
main = "app"

[build]
run = "CGO_ENABLED=0 go build -o app ."

[vars.development]
APP_NAME = "Dev"

[vars.production]
APP_NAME = "Production"

[envs.development]
route = "my-app.tako.test"

[envs.production]
route = "my-app.example.com"
servers = ["my-server"]
```

## Step 3: Write Your App

### net/http (standard library)

```go
package main

import (
	"fmt"
	"net/http"
	"os"
	"tako.sh"
)

func main() {
	mux := http.NewServeMux()
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		fmt.Fprint(w, "Hello from Tako!")
	})

	if err := tako.ListenAndServe(mux); err != nil {
		fmt.Fprintf(os.Stderr, "server error: %v\n", err)
		os.Exit(1)
	}
}
```

### Gin

```go
package main

import (
	"github.com/gin-gonic/gin"
	"tako.sh"
)

func main() {
	r := gin.Default()
	r.GET("/", func(c *gin.Context) {
		c.String(200, "Hello from Tako!")
	})
	tako.ListenAndServe(r) // *gin.Engine implements http.Handler
}
```

### Echo

```go
package main

import (
	"net/http"
	"github.com/labstack/echo/v4"
	"tako.sh"
)

func main() {
	e := echo.New()
	e.GET("/", func(c echo.Context) error {
		return c.String(http.StatusOK, "Hello from Tako!")
	})
	tako.ListenAndServe(e) // *echo.Echo implements http.Handler
}
```

### Chi

```go
package main

import (
	"fmt"
	"net/http"
	"github.com/go-chi/chi/v5"
	"tako.sh"
)

func main() {
	r := chi.NewRouter()
	r.Get("/", func(w http.ResponseWriter, r *http.Request) {
		fmt.Fprint(w, "Hello from Tako!")
	})
	tako.ListenAndServe(r) // chi.Mux implements http.Handler
}
```

## Step 4: Configure Secrets

Secrets are managed via the Tako CLI:

```bash
tako secrets set DATABASE_URL "postgres://..."
tako secrets set API_KEY "sk-..."
```

Generate typed access:

```bash
tako typegen
```

This creates `tako_secrets.go` with a typed `Secrets` struct:

```go
db := Secrets.DatabaseUrl()
key := Secrets.ApiKey()
```

Secrets are injected at runtime by tako-server, not baked into the binary.

## Step 5: Deploy

```bash
tako deploy
```

This builds locally (cross-compiles with `CGO_ENABLED=0` for the target server), uploads the binary to your Tako server, and performs a rolling update.

## Build Stages (Monorepos)

For monorepo projects with multiple build steps:

```toml
main = "cmd/server/app"

[[build_stages]]
name = "shared"
run = "go generate ./pkg/..."
cwd = "."

[[build_stages]]
name = "server"
run = "CGO_ENABLED=0 go build -o app ."
cwd = "cmd/server"
```

`cwd` allows `..` for monorepo traversal (guarded against root escape).
`[[build_stages]]` is mutually exclusive with `[build].run`.

## Local Development

```bash
tako dev
```

This runs your app locally with:

- `go run .` with file watching (`**/*.go`, `go.mod`, `go.sum`)
- Local HTTPS via auto-generated certificates
- `.tako.test` domain for local development
- Automatic restart on file changes

## Common Mistakes

### 1. CRITICAL: Not using the SDK

```go
// WRONG — app won't respond to Tako health checks or receive secrets
http.ListenAndServe(":3000", mux)

// CORRECT — use Tako SDK
tako.ListenAndServe(mux)
```

### 2. CRITICAL: Missing CGO_ENABLED=0 in build

```toml
# WRONG — may produce a dynamically linked binary that fails on the server
[build]
run = "go build -o app ."

# CORRECT — static binary for cross-platform deployment
[build]
run = "CGO_ENABLED=0 go build -o app ."
```

### 3. HIGH: Wrong main value

```toml
# WRONG — main should be the binary name, not a source file
main = "main.go"

# CORRECT — the compiled binary name
main = "app"
```

### 4. HIGH: Hardcoding secrets in source

```go
// WRONG — secrets in code
const dbURL = "postgres://user:pass@host/db"

// CORRECT — use Tako secrets at request time
func handler(w http.ResponseWriter, r *http.Request) {
    dbURL := Secrets.DatabaseUrl()
    // ...
}
```

### 5. MEDIUM: Forgetting to run tako typegen

After adding or removing secrets via `tako secrets set/rm`, run `tako typegen` to regenerate `tako_secrets.go`. The generated file should be committed to source control.

## Cross-References

- [tako-sdk](../../tako-sdk/SKILL.md) — SDK API reference, ListenAndServe, Listener, secrets
