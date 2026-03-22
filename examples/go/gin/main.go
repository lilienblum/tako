package main

import (
	"fmt"
	"net/http"
	"os"

	"github.com/gin-gonic/gin"
	"tako.sh"
)

func main() {
	r := gin.Default()

	r.GET("/", func(c *gin.Context) {
		c.HTML(http.StatusOK, "", nil)
		c.Writer.WriteString(fmt.Sprintf(`<!doctype html>
<html>
<body>
  <h1>Gin + Tako</h1>
  <p>PID: %d</p>
</body>
</html>`, os.Getpid()))
	})

	r.GET("/api/health", func(c *gin.Context) {
		c.JSON(http.StatusOK, gin.H{"ok": true})
	})

	r.GET("/api/secret", func(c *gin.Context) {
		c.JSON(http.StatusOK, gin.H{
			"has_secret": Secrets.ExampleSecret() != "",
		})
	})

	if err := tako.ListenAndServe(r); err != nil {
		fmt.Fprintf(os.Stderr, "server error: %v\n", err)
		os.Exit(1)
	}
}
