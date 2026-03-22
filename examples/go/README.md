# Go Examples

- `basic/` — net/http
- `gin/` — [Gin](https://github.com/gin-gonic/gin)
- `echo/` — [Echo](https://echo.labstack.com/)
- `chi/` — [Chi](https://github.com/go-chi/chi)

## Run

```bash
just tako examples/go/basic dev
```

Or directly:

```bash
cd examples/go/basic
go run .
```

## Secrets

Each example has pre-configured encrypted secrets. Passphrase: `tako-example`

```bash
tako -c examples/go/basic/tako.toml secrets list --env development
```
