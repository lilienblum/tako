package main

import (
	"fmt"
	"net/http"
	"os"

	"github.com/labstack/echo/v4"
	"github.com/labstack/echo/v4/middleware"
	"tako.sh"
)

func main() {
	e := echo.New()
	e.Use(middleware.Logger())
	e.Use(middleware.Recover())

	e.GET("/", func(c echo.Context) error {
		html := fmt.Sprintf(`<!doctype html>
<html>
<body>
  <h1>Echo + Tako</h1>
  <p>PID: %d</p>
</body>
</html>`, os.Getpid())
		return c.HTML(http.StatusOK, html)
	})

	e.GET("/api/health", func(c echo.Context) error {
		return c.JSON(http.StatusOK, map[string]bool{"ok": true})
	})

	e.GET("/api/secret", func(c echo.Context) error {
		return c.JSON(http.StatusOK, map[string]bool{
			"has_secret": Secrets.ExampleSecret() != "",
		})
	})

	if err := tako.ListenAndServe(e); err != nil {
		fmt.Fprintf(os.Stderr, "server error: %v\n", err)
		os.Exit(1)
	}
}
