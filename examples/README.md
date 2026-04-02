# Examples

Sample applications for running Tako end-to-end.

## Available Examples

### JavaScript

- `javascript/demo/`: minimal Bun fetch-handler app integrated with `tako.sh`.
- `javascript/tanstack-start/`: TanStack Start `start-basic`-style app with `tako.sh/vite` server-entry wrapping.

### Go

- `go/basic/`: minimal Go HTTP app using `net/http` + `tako.sh` SDK.
- `go/gin/`: [Gin](https://github.com/gin-gonic/gin) app with Tako.
- `go/echo/`: [Echo](https://echo.labstack.com/) app with Tako.
- `go/chi/`: [Chi](https://github.com/go-chi/chi) app with Tako.

## Run Example via Tako

From repository root:

```bash
just tako examples/javascript/demo dev
just tako examples/go/basic dev
```

Then open the URL shown by `tako dev`.

## More Details

- `examples/javascript/README.md`
- `examples/go/README.md`
